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

    /// Every state name — the `delivery ls --state` vocabulary
    /// (CAD-437).
    pub fn all_str() -> Vec<&'static str> {
        [
            State::Working,
            State::Reviewing,
            State::Unstaffed,
            State::Passed,
            State::Enqueued,
            State::Escalated,
            State::Merged,
            State::Declined,
            State::Closed,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect()
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
    /// When the GitHub read began (epoch secs) — set by the observing
    /// process before `gh pr view`. An observation applied later whose
    /// `read_at` predates the record's last head change is stale: the
    /// "moved" head it shows is the one that change already recorded.
    #[serde(default)]
    pub read_at: i64,
}

/// CAD-449: what a merge did to the ticket's tracker status. Written
/// once, on the transition into [`State::Merged`]; `pending` is retried
/// by the router pass at most twice, then waits on the operator.
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
        /// Initial failure counts as one; two router retries are allowed.
        #[serde(default = "initial_done_attempts")]
        attempts: u8,
    },
}

pub(crate) const MAX_DONE_ATTEMPTS: u8 = 3;

fn initial_done_attempts() -> u8 {
    1
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
    /// When `head` was last set (epoch secs) — by the worker's done
    /// report or a confirmed post-review move. An observation whose
    /// `read_at` is earlier saw the head that change recorded, not a
    /// newer one (CAD-564).
    #[serde(default)]
    pub head_at: i64,
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
    /// CAD-776: how often the record has become merge-ready. Bumped
    /// under `delivery_lock` on the transition into merge-ready, saved
    /// with the observation, so the wake key derives from persisted
    /// record state only — `delivery_lock` serializes observers, and
    /// every post-save wake attempt reads the same saved key, which
    /// the message id dedupes to one wake.
    #[serde(default)]
    pub ready_epoch: u32,
    /// CAD-362: the diff-size risk class measured when this head was
    /// routed (or last classified). `oversized` means the record sits
    /// unstaffed waiting for the worker to split the diff, not for a
    /// reviewer — the note says so.
    #[serde(default)]
    pub risk: Option<String>,
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
            head_at: 0,
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
            ready_epoch: 0,
            risk: None,
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

    /// CAD-776: after an observation is applied, count the transition
    /// into merge-ready. Runs under `delivery_lock` before the save,
    /// so the epoch persists atomically with the observation that
    /// caused it; a steady observation bumps nothing.
    pub fn advance_ready_epoch(&mut self, was_ready: bool) {
        if !was_ready && self.merge_ready() {
            self.ready_epoch = self.ready_epoch.saturating_add(1);
        }
    }

    /// CAD-776: the wake key for the current readiness streak —
    /// `{issue}/{reviewed sha}@{epoch}` — when merge-ready now. A
    /// replay recomputes the same key, a regression then recovery
    /// bumps the epoch, and a moved head keys under its own sha.
    pub fn ready_key(&self) -> Option<String> {
        if !self.merge_ready() {
            return None;
        }
        Some(format!(
            "{}/{}@{}",
            self.issue,
            self.passed_sha()?,
            self.ready_epoch
        ))
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

/// CAD-362: the diff-size tier a review runs under. `small` is the
/// ordinary kickoff; `heavy` annotates it so the reviewer reads with
/// more care; `oversized` never reaches a reviewer — the diff is
/// flagged for splitting first, because a reviewer skims a 40-file
/// diff and the verdict stops meaning anything. Measured from the
/// PR's `additions + deletions` and `changedFiles` — the same fields
/// an observation already records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    Small,
    Heavy,
    Oversized,
}

/// Diff-size thresholds. A review over `OVERSIZED_*` cannot be
/// diligent — flagged for splitting before a reviewer is asked.
pub const OVERSIZED_FILES: u64 = 40;
pub const OVERSIZED_LINES: u64 = 3000;
/// `HEAVY_*` annotates the kickoff so the reviewer slows down.
pub const HEAVY_FILES: u64 = 10;
pub const HEAVY_LINES: u64 = 800;

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Small => "small",
            Risk::Heavy => "heavy",
            Risk::Oversized => "oversized",
        }
    }
}

/// Classify a diff's review risk from `additions + deletions` lines
/// and `changedFiles` — the fields `gh pr view` and the record's
/// `observed` both carry.
pub fn risk_class(additions: u64, deletions: u64, files: u64) -> Risk {
    let lines = additions + deletions;
    if files > OVERSIZED_FILES || lines > OVERSIZED_LINES {
        Risk::Oversized
    } else if files > HEAVY_FILES || lines > HEAVY_LINES {
        Risk::Heavy
    } else {
        Risk::Small
    }
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

/// Launch role that [`pick_reviewer`] will route a review to (CAD-591).
/// `pm` and `worker` stay the authorization roles; `reviewer` is only
/// this designation.
pub const REVIEWER_ROLE: &str = "reviewer";

/// One agent the reviewer rule may choose.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub alias: String,
    pub provider: String,
    /// The model the endpoint is running, when registered — the
    /// vendor half of the cross-vendor rule: two `pi` agents on
    /// `devin/…` and `opencode-go/…` models are different vendors.
    pub model: Option<String>,
    pub state: String,
    pub enabled: bool,
    /// The agent's PM (`params.upstream`), when it is a group member.
    pub upstream: Option<String>,
    /// Launch role stored at registration (`pm`, `worker`, or `reviewer`).
    pub role: String,
}

/// The vendor a `(provider, model)` pair counts as. A model names its
/// vendor in its namespace: `devin/swe-2-high` → `devin`,
/// `opencode-go/muse-spark` → `opencode-go`. A transport namespace that
/// itself proxies vendors carries it one segment deeper:
/// `openrouter/z-ai/glm-5.3` → `z-ai`. A bare model name or no model
/// at all resolves to the launch provider.
pub fn vendor(provider: &str, model: Option<&str>) -> String {
    let Some(model) = model else {
        return provider.to_string();
    };
    let segs: Vec<&str> = model.split('/').collect();
    match segs.as_slice() {
        [transport, vendor, ..] if *transport == "openrouter" => vendor.to_string(),
        [vendor, ..] if segs.len() > 1 => vendor.to_string(),
        _ => provider.to_string(),
    }
}

/// The pm.yaml `review:` section — the operator's catalog override for
/// reviewer pairing (CAD-340/362). `pair` pins an author's reviews to
/// one reviewer when that reviewer qualifies; `never` bars a pair
/// outright. Both key on the worker's alias. Empty means the measured
/// rules decide everything.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReviewRules {
    /// `worker-alias: reviewer-alias` — always this reviewer when it
    /// qualifies.
    #[serde(default)]
    pub pair: std::collections::BTreeMap<String, String>,
    /// `worker-alias: [reviewer-alias…]` — pairs that never happen.
    #[serde(default)]
    pub never: std::collections::BTreeMap<String, Vec<String>>,
}

/// Why a reviewer was chosen — recorded on the ticket and the
/// `review_routed` event so a same-vendor fallback is never silent
/// (CAD-340's "collision launches the role fallback and records the
/// reason").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickReason {
    /// The previous round's reviewer kept the ticket.
    Held,
    /// pm.yaml `review.pair` pinned this worker's reviews to it.
    Catalog,
    /// A prior convergence on this worker's tickets outranked the
    /// vendor heuristic.
    ProvenPair,
    /// A different vendor from the worker's.
    CrossVendor,
    /// No cross-vendor reviewer was idle — the vendor rule fell back.
    SameVendorFallback,
    /// The worker's vendor could not be determined — vendor preference
    /// had nothing to compare against.
    VendorUnknown,
}

impl PickReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PickReason::Held => "held",
            PickReason::Catalog => "catalog-pair",
            PickReason::ProvenPair => "proven-pair",
            PickReason::CrossVendor => "cross-vendor",
            PickReason::SameVendorFallback => "same-vendor-fallback",
            PickReason::VendorUnknown => "vendor-unknown",
        }
    }

    /// The fallback forms CAD-340 wants recorded on the task.
    pub fn is_fallback(self) -> bool {
        matches!(
            self,
            PickReason::SameVendorFallback | PickReason::VendorUnknown
        )
    }

    /// The phrase the routed comment and record note carry.
    pub fn describe(self) -> &'static str {
        match self {
            PickReason::Held => "kept from the previous round",
            PickReason::Catalog => "pinned by pm.yaml review.pair",
            PickReason::ProvenPair => "proven pair record",
            PickReason::CrossVendor => "cross-vendor",
            PickReason::SameVendorFallback => {
                "same-vendor fallback — no cross-vendor reviewer was idle"
            }
            PickReason::VendorUnknown => "vendor unknown — the worker registered no model",
        }
    }
}

/// The pick's answer: the reviewer alias plus why it was chosen.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub alias: String,
    pub reason: PickReason,
}

/// How this (worker, reviewer) pair has done before — measured
/// precision from `delivery.json` history, per the CAD-340 rule that
/// pairings are chosen by measured review precision, not just vendor:
///
/// - a PASS on this worker's ticket that the merge queue upheld
///   (the record reached `merged`) is worth the most;
/// - a PASS awaiting its merge still counts;
/// - a ticket `escalated` while this reviewer held it, or one where
///   the reviewer is in `excluded` (it was on duty when the head moved
///   without the worker — its review may have covered its own push),
///   count against the pair.
///
/// Scoreless pairs are `0` — history only demotes proven-bad and
/// rewards proven-good; it never locks out an unpaired reviewer.
pub fn pair_score(worker: &str, reviewer: &str, history: &[&Record]) -> i32 {
    let mut score = 0;
    for r in history.iter().filter(|r| r.worker == worker) {
        if r.excluded.iter().any(|e| e == reviewer) {
            score -= 3;
        }
        if r.state == State::Escalated && r.reviewer.as_deref() == Some(reviewer) {
            score -= 2;
        }
        if r.verdict
            .as_ref()
            .is_some_and(|v| v.reviewer == reviewer && v.verdict == "pass")
        {
            score += match r.state {
                State::Merged => 2,
                State::Declined => -2,
                _ => 1,
            };
        }
    }
    score
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

/// The worker's PM: its `upstream`. Groups are one level deep, so that
/// alias is the group. A root worker has none.
fn group_pm<'a>(alias: &str, agents: &'a [Candidate]) -> Option<&'a str> {
    agents
        .iter()
        .find(|a| a.alias == alias)
        .and_then(|a| a.upstream.as_deref())
}

/// `candidate` sits in a PM group that is not the worker's. A root
/// (no upstream) is the independent pool, not another PM's group.
fn foreign_pm(worker_pm: Option<&str>, candidate: &Candidate) -> bool {
    match candidate.upstream.as_deref() {
        Some(up) => worker_pm != Some(up),
        None => false,
    }
}

/// The reviewer for a worker's head (CAD-591, CAD-362). Only an agent
/// whose launch role is [`REVIEWER_ROLE`]. Never the worker or anyone
/// in its group line ([`worker_group`]), never a member of another
/// PM's group, never the master, never an alias in `exclude` or in the
/// catalog's `never` list for this worker, never a disabled, fenced
/// (`attention`) or inbox agent. An implementer (`worker` / `pm`) is
/// never chosen, busy or idle. A fresh review goes only to an idle
/// reviewer. When none is idle the review stays unassigned. The
/// previous round's reviewer keeps the ticket while it still qualifies
/// and is idle; a busy holder yields when an idle reviewer can take
/// the ticket — it stays only when every qualifying reviewer is busy,
/// so the verdicts on that head stay comparable. A catalog `pair` pin
/// for the worker wins next. Among the rest, the measured pair score
/// ([`pair_score`]) ranks first — a proven pairing outranks vendor
/// heuristics — then the cross-vendor rule: a reviewer whose model
/// vendor differs from the worker's wins over a same-vendor one, then
/// alias order. The returned [`Pick`] records why — a same-vendor
/// fallback is never silent. A caller passes `None` for `previous`
/// (and the old reviewer in `exclude`) when the head moved without the
/// worker, since whoever pushed must not review its own commits.
pub fn pick_reviewer(
    worker: &str,
    worker_vendor: Option<&str>,
    previous: Option<&str>,
    exclude: &[String],
    agents: &[Candidate],
    rules: &ReviewRules,
    history: &[&Record],
) -> Option<Pick> {
    let group = worker_group(worker, agents);
    let worker_pm = group_pm(worker, agents);
    let never: &[String] = rules.never.get(worker).map(Vec::as_slice).unwrap_or(&[]);
    let qualifies = |a: &&Candidate| {
        a.role == REVIEWER_ROLE
            && a.enabled
            && a.state != "attention"
            && a.provider != "inbox"
            && !group.contains(&a.alias)
            && !exclude.contains(&a.alias)
            && !never.contains(&a.alias)
            && !crate::master::is_master(&a.alias)
            && !foreign_pm(worker_pm, a)
    };
    let eligible: Vec<&Candidate> = agents.iter().filter(qualifies).collect();
    let idle_exists = eligible.iter().any(|a| a.state == "idle");
    if let Some(prev) = previous {
        if let Some(held) = eligible.iter().find(|a| a.alias == prev) {
            // A busy holder yields when an idle reviewer can take over.
            if held.state == "idle" || !idle_exists {
                return Some(Pick {
                    alias: prev.to_string(),
                    reason: PickReason::Held,
                });
            }
        }
    }
    if let Some(pin) = rules.pair.get(worker) {
        if let Some(pinned) = eligible.iter().find(|a| &a.alias == pin) {
            // Same rule as the previous round's holder: a busy pin
            // yields to an idle reviewer rather than stall the review.
            if pinned.state == "idle" || !idle_exists {
                return Some(Pick {
                    alias: pin.clone(),
                    reason: PickReason::Catalog,
                });
            }
        }
    }
    let cross = |c: &Candidate| {
        worker_vendor.is_some_and(|wv| vendor(&c.provider, c.model.as_deref()) != wv)
    };
    let mut ranked: Vec<(i32, bool, &Candidate)> = eligible
        .into_iter()
        .filter(|a| a.state == "idle")
        .map(|a| (pair_score(worker, &a.alias, history), cross(a), a))
        .collect();
    // Measured pair score first, then cross-vendor, then alias —
    // all descending except the alias tiebreak.
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(a.2.alias.cmp(&b.2.alias))
    });
    let (score, is_cross, chosen) = ranked.first()?;
    let reason = if worker_vendor.is_none() {
        PickReason::VendorUnknown
    } else if *score > 0 {
        PickReason::ProvenPair
    } else if *is_cross {
        PickReason::CrossVendor
    } else {
        PickReason::SameVendorFallback
    };
    Some(Pick {
        alias: chosen.alias.clone(),
        reason,
    })
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
/// take it as it is. It carries the PR, the head, the risk tier
/// (CAD-362), the acceptance criteria and the pinning rules. When the
/// criteria do not fit `ceiling`, the line points at the ticket file
/// instead of cutting them.
#[allow(clippy::too_many_arguments)]
pub fn review_kickoff(
    issue: &str,
    round: u32,
    pr: &str,
    sha: &str,
    worker: &str,
    risk: Option<&str>,
    acceptance: Option<&str>,
    note: &Path,
    ceiling: usize,
) -> String {
    let build = |criteria: &str| {
        let risk_clause = risk.map(|r| format!("Risk: {r}. ")).unwrap_or_default();
        format!(
            "[review] {issue} round {round}: independently review PR {pr} at head {sha} \
             (worker {worker}; ticket {note}). {criteria}{risk_clause}Rules: judge exactly \
             this head — \
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
    // CAD-564: the read starts now — whatever this call returns was
    // true at `read_at`, and a record change applied after it (a done
    // report's new head) is newer than anything the observation shows.
    let read_at = crate::issue::time::now_epoch();
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
        "read_at": read_at,
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

pub(crate) fn gh(gh_bin: &str, args: &[&str]) -> Result<String> {
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

    #[test]
    fn legacy_pending_done_write_retains_its_failed_attempt() {
        let old: TicketDone = serde_json::from_value(json!({
            "outcome": "pending", "why": "commit refused", "from": "doing"
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(old).unwrap()["attempts"], 1);
    }

    fn cand(alias: &str, provider: &str) -> Candidate {
        Candidate {
            alias: alias.into(),
            provider: provider.into(),
            model: None,
            state: "idle".into(),
            enabled: true,
            upstream: None,
            role: REVIEWER_ROLE.into(),
        }
    }

    /// `pick_reviewer` with no catalog and no history — most routing
    /// tests want only the chosen alias.
    fn pick(
        worker: &str,
        worker_vendor: Option<&str>,
        previous: Option<&str>,
        exclude: &[String],
        agents: &[Candidate],
    ) -> Option<String> {
        pick_reviewer(
            worker,
            worker_vendor,
            previous,
            exclude,
            agents,
            &ReviewRules::default(),
            &[],
        )
        .map(|p| p.alias)
    }

    /// The pick and its reason together.
    fn pick_why(
        worker: &str,
        worker_vendor: Option<&str>,
        agents: &[Candidate],
        rules: &ReviewRules,
        history: &[&Record],
    ) -> Option<Pick> {
        pick_reviewer(worker, worker_vendor, None, &[], agents, rules, history)
    }

    fn staff(alias: &str, role: &str, state: &str, upstream: Option<&str>) -> Candidate {
        Candidate {
            role: role.into(),
            state: state.into(),
            upstream: upstream.map(str::to_string),
            ..cand(alias, "codex")
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
            pick("w1", Some("claude"), None, &[], &agents).as_deref(),
            Some("z-codex")
        );
        // None staffed: another session of the same provider.
        let same = vec![
            cand("master", "claude"),
            cand("w1", "claude"),
            cand("r", "claude"),
        ];
        assert_eq!(
            pick("w1", Some("claude"), None, &[], &same).as_deref(),
            Some("r")
        );
        // Never the worker, never the master.
        let alone = vec![cand("master", "codex"), cand("w1", "claude")];
        assert_eq!(pick("w1", Some("claude"), None, &[], &alone), None);
        // Fenced or disabled agents are skipped; the previous reviewer
        // keeps the ticket while it qualifies.
        let mut fenced = cand("z-codex", "codex");
        fenced.state = "attention".into();
        let agents = vec![cand("a-claude", "claude"), fenced, cand("w1", "claude")];
        assert_eq!(
            pick("w1", Some("claude"), None, &[], &agents).as_deref(),
            Some("a-claude")
        );
        let agents = vec![cand("a", "codex"), cand("b", "codex"), cand("w1", "claude")];
        assert_eq!(
            pick("w1", Some("claude"), Some("b"), &[], &agents).as_deref(),
            Some("b")
        );
        // A previous reviewer that is the worker now is not reused.
        assert_eq!(
            pick("w1", Some("claude"), Some("w1"), &[], &agents).as_deref(),
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
            pick("w1", Some("claude"), None, &[], &agents).as_deref(),
            Some("sib")
        );
        // Nor as the sticky previous reviewer.
        assert_eq!(
            pick("w1", Some("claude"), Some("a1"), &[], &agents).as_deref(),
            Some("sib")
        );
        // A reviewer excluded (it pushed the moved head) is skipped.
        assert_eq!(
            pick("w1", Some("claude"), None, &["sib".into()], &agents),
            None
        );
        // An upstream cycle ends.
        let cyc = vec![with_up("w1", "x"), with_up("x", "w1"), cand("r", "codex")];
        assert_eq!(pick("w1", None, None, &[], &cyc).as_deref(), Some("r"));
    }

    /// CAD-591: the alias that would win under the old "any peer" rule
    /// is refused when it is an implementer, busy, or in another PM's
    /// group. Each bad candidate sorts ahead of `rev`, so a missing
    /// guard picks that candidate instead.
    #[test]
    fn review_goes_only_to_an_idle_designated_reviewer() {
        let agents = vec![
            staff("w1", "worker", "idle", Some("pm")),
            staff("pm", "pm", "idle", None),
            // Idle implementer in the author's group. Alias first.
            staff("a-impl", "worker", "idle", Some("pm")),
            // Busy implementer — the CAD-584 failure.
            staff("b-busy-impl", "worker", "busy", Some("pm")),
            // Designated, but busy. A fresh review never lands here.
            // Sticky reuse is refused too while an idle reviewer remains.
            staff("c-busy-rev", "reviewer", "busy", Some("pm")),
            // Designated and idle, but another PM's group.
            staff("d-foreign", "reviewer", "idle", Some("other-pm")),
            // The one agent that qualifies in the author's PM group.
            staff("rev", "reviewer", "idle", Some("pm")),
            // A root reviewer also qualifies; alias order keeps `rev`.
            staff("z-root", "reviewer", "idle", None),
        ];
        assert_eq!(
            pick("w1", Some("claude"), None, &[], &agents).as_deref(),
            Some("rev")
        );
        // The author is not eligible even when their own role is reviewer.
        let mut author = agents.clone();
        author.iter_mut().find(|a| a.alias == "w1").unwrap().role = "reviewer".into();
        assert_eq!(
            pick("w1", Some("claude"), None, &[], &author).as_deref(),
            Some("rev")
        );
        // Sticky previous reviewer that no longer qualifies is not reused.
        assert_eq!(
            pick("w1", Some("claude"), Some("d-foreign"), &[], &agents).as_deref(),
            Some("rev")
        );
        assert_eq!(
            pick("w1", Some("claude"), Some("b-busy-impl"), &[], &agents).as_deref(),
            Some("rev")
        );
        // Sticky previous is busy, and an idle reviewer exists: hand it
        // to the idle one, not back to the busy holder.
        assert_eq!(
            pick("w1", Some("claude"), Some("c-busy-rev"), &[], &agents).as_deref(),
            Some("rev")
        );
        // No idle reviewer: a fresh review stays unassigned. The busy
        // holder keeps a ticket it already has, because nobody idle can
        // take over. An implementer is not a fallback either way.
        let busy_only: Vec<_> = agents
            .iter()
            .filter(|a| a.alias != "rev" && a.alias != "z-root")
            .cloned()
            .collect();
        assert_eq!(
            pick("w1", Some("claude"), None, &[], &busy_only).as_deref(),
            None
        );
        assert_eq!(
            pick("w1", Some("claude"), Some("c-busy-rev"), &[], &busy_only).as_deref(),
            Some("c-busy-rev")
        );
        let implementers: Vec<_> = busy_only
            .into_iter()
            .filter(|a| a.role != REVIEWER_ROLE)
            .collect();
        assert_eq!(pick("w1", Some("claude"), None, &[], &implementers), None);
    }

    /// A record whose reviewer filed a PASS, in `state`.
    fn passed(worker: &str, rev: &str, state: State) -> Record {
        let mut r = Record::new("CAD-1", "demo", worker, 0);
        r.state = state;
        r.reviewer = Some(rev.into());
        r.verdict = Some(VerdictRec {
            verdict: "pass".into(),
            sha: "s".repeat(40),
            reviewer: rev.into(),
            summary: String::new(),
            report: "CAD-1/reports/x.md".into(),
            at: 0,
        });
        r
    }

    #[test]
    fn vendor_comes_from_the_model_namespace() {
        assert_eq!(vendor("pi", Some("devin/swe-2-high")), "devin");
        assert_eq!(vendor("pi", Some("opencode-go/muse-spark")), "opencode-go");
        // A transport that proxies vendors carries its own segment.
        assert_eq!(vendor("pi", Some("openrouter/z-ai/glm-5.3")), "z-ai");
        // Bare model or none: the launch provider is the vendor.
        assert_eq!(vendor("pi", Some("glm-5.3")), "pi");
        assert_eq!(vendor("pi", None), "pi");
    }

    /// CAD-362: two agents on the same launch provider are different
    /// vendors when their models differ — the model namespace, not the
    /// transport, is what the rule compares.
    #[test]
    fn the_model_namespace_decides_cross_vendor() {
        let mut a = cand("a-same", "pi");
        a.model = Some("devin/swe-2-high".into());
        let mut z = cand("z-cross", "pi");
        z.model = Some("opencode-go/muse-spark".into());
        let agents = vec![a, cand("w1", "pi"), z];
        // Alias order prefers a-same; the vendor rule overrides it.
        let p = pick_why("w1", Some("devin"), &agents, &ReviewRules::default(), &[]).unwrap();
        assert_eq!(p.alias, "z-cross");
        assert_eq!(p.reason, PickReason::CrossVendor);
    }

    /// The pm.yaml catalog: `pair` pins, `never` bars (CAD-340).
    #[test]
    fn the_catalog_pins_and_bars_pairs() {
        let mut rules = ReviewRules::default();
        rules.pair.insert("w1".into(), "pin".into());
        rules.never.insert("w1".into(), vec!["barred".into()]);
        let agents = vec![
            cand("barred", "codex"),
            cand("pin", "codex"),
            cand("other", "codex"),
            cand("w1", "claude"),
        ];
        let p = pick_why("w1", Some("claude"), &agents, &rules, &[]).unwrap();
        assert_eq!((p.alias.as_str(), p.reason), ("pin", PickReason::Catalog));
        // `never` bars even when it is the catalog pin's only rival —
        // and a barred pin itself cannot win.
        rules.pair.insert("w1".into(), "barred".into());
        let p = pick_why("w1", Some("claude"), &agents, &rules, &[]).unwrap();
        assert_eq!(p.alias, "other");
        // Only the barred reviewer remains: unassigned, never barred-in.
        let thin = vec![cand("barred", "codex"), cand("w1", "claude")];
        assert_eq!(pick_why("w1", Some("claude"), &thin, &rules, &[]), None);
        // A busy pin yields to an idle reviewer rather than stall.
        let mut rules = ReviewRules::default();
        rules.pair.insert("w1".into(), "pin".into());
        let mut agents = vec![cand("pin", "codex"), cand("other", "codex")];
        agents[0].state = "busy".into();
        agents.push(cand("w1", "claude"));
        let p = pick_why("w1", Some("claude"), &agents, &rules, &[]).unwrap();
        assert_eq!(p.alias, "other");
    }

    /// A measured-good pairing outranks the cross-vendor rule, and a
    /// measured-bad one loses to it (CAD-340's measured precision).
    #[test]
    fn measured_pair_history_ranks_above_vendor() {
        let mut a = cand("a-same", "pi");
        a.model = Some("devin/swe-2-high".into());
        let mut z = cand("z-cross", "pi");
        z.model = Some("opencode-go/muse-spark".into());
        let agents = vec![a, z, cand("w1", "pi")];
        // a-same twice reviewed w1 to a merge — the pair is proven.
        let good1 = passed("w1", "a-same", State::Merged);
        let good2 = passed("w1", "a-same", State::Merged);
        let hist: Vec<&Record> = vec![&good1, &good2];
        let p = pick_why("w1", Some("devin"), &agents, &ReviewRules::default(), &hist).unwrap();
        assert_eq!(
            (p.alias.as_str(), p.reason),
            ("a-same", PickReason::ProvenPair)
        );
        // The same reviewer whose record is bad loses to cross-vendor:
        // it was on duty when a head moved (excluded), or escalated.
        let mut bad = passed("w1", "a-same", State::Escalated);
        bad.excluded.push("a-same".into());
        let hist: Vec<&Record> = vec![&bad];
        let p = pick_why("w1", Some("devin"), &agents, &ReviewRules::default(), &hist).unwrap();
        assert_eq!(
            (p.alias.as_str(), p.reason),
            ("z-cross", PickReason::CrossVendor)
        );
        // A PASS whose merge the operator declined counts against.
        let declined = passed("w1", "a-same", State::Declined);
        let hist: Vec<&Record> = vec![&declined];
        let p = pick_why("w1", Some("devin"), &agents, &ReviewRules::default(), &hist).unwrap();
        assert_eq!(p.alias, "z-cross");
    }

    /// CAD-340: a fallback is never silent — the pick says why.
    #[test]
    fn a_same_vendor_fallback_records_its_reason() {
        let mut a = cand("a-same", "pi");
        a.model = Some("devin/swe-2-high".into());
        let agents = vec![a, cand("w1", "pi")];
        let p = pick_why("w1", Some("devin"), &agents, &ReviewRules::default(), &[]).unwrap();
        assert_eq!(p.alias, "a-same");
        assert_eq!(p.reason, PickReason::SameVendorFallback);
        assert!(p.reason.is_fallback());
        // And when the worker's own vendor is unknown — never
        // registered, no model — the pick says that instead.
        let p = pick_why("w1", None, &agents, &ReviewRules::default(), &[]).unwrap();
        assert_eq!(p.reason, PickReason::VendorUnknown);
    }

    #[test]
    fn risk_tiers_classify_the_diff() {
        assert_eq!(risk_class(10, 10, 3), Risk::Small);
        assert_eq!(risk_class(400, 400, 9), Risk::Small);
        assert_eq!(risk_class(0, 0, HEAVY_FILES + 1), Risk::Heavy);
        assert_eq!(risk_class(HEAVY_LINES + 1, 0, 3), Risk::Heavy);
        assert_eq!(risk_class(400, 401, 3), Risk::Heavy);
        assert_eq!(risk_class(0, 0, OVERSIZED_FILES + 1), Risk::Oversized);
        assert_eq!(risk_class(OVERSIZED_LINES, 1, 3), Risk::Oversized);
        // The boundary values themselves stay in the lower tier.
        assert_eq!(risk_class(HEAVY_LINES, 0, HEAVY_FILES), Risk::Small);
        assert_eq!(risk_class(OVERSIZED_LINES, 0, OVERSIZED_FILES), Risk::Heavy);
    }

    /// `risk` rides the record file and defaults absent for records
    /// written before CAD-362.
    #[test]
    fn risk_round_trips_and_defaults_absent() {
        let mut r = Record::new("CAD-1", "demo", "w1", 0);
        r.risk = Some("heavy".into());
        let back: Record = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(back.risk.as_deref(), Some("heavy"));
        let mut v = serde_json::to_value(&r).unwrap();
        v.as_object_mut().unwrap().remove("risk");
        let back: Record = serde_json::from_value(v).unwrap();
        assert_eq!(back.risk, None);
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
            Some("heavy"),
            Some("1) [ ] \"x\""),
            note,
            4000,
        );
        assert!(!k.contains('\n'), "{k}");
        assert!(k.contains(&sha) && k.contains("pull/7") && k.contains("1) [ ] \"x\""));
        assert!(k.contains("--kind verdict"), "{k}");
        assert!(k.contains("Risk: heavy."), "{k}");
        let long = "y".repeat(5000);
        let k = review_kickoff("D-2", 1, "u", &sha, "w1", None, Some(&long), note, 4000);
        assert!(!k.contains(&long));
        assert!(k.contains("/pm/demo/D-2/issue.md"), "{k}");
    }

    /// CAD-776, deterministic concurrency proof: `delivery_lock`
    /// serializes observers, so B always loads A's saved record —
    /// never the same pre-transition base twice. A transitions (epoch
    /// 0→1); B's steady observation bumps nothing; then the post-lock
    /// wakes run B-before-A and both use the identical persisted key,
    /// so the message id dedupes them to one wake no matter the wake
    /// order.
    #[test]
    fn ready_epoch_converges_for_serialized_observers() {
        let sha = "a".repeat(40);
        let mut base = Record::new("D-2", "demo", "w1", 0);
        base.state = State::Passed;
        base.verdict = Some(VerdictRec {
            verdict: "pass".into(),
            sha: sha.clone(),
            reviewer: "r1".into(),
            summary: "ok".into(),
            report: "D-2/reports/x.md".into(),
            at: 0,
        });
        let green = Observed {
            head: sha.clone(),
            pr_state: "OPEN".into(),
            ci_green: true,
            ..Observed::default()
        };
        // The base record is not ready: no key yet.
        assert!(!base.merge_ready());
        assert_eq!(base.ready_key(), None);
        // Observer A transitions under the lock and saves epoch 1.
        let mut a = base.clone();
        let was_a = a.merge_ready();
        assert!(!was_a);
        a.observed = Some(green.clone());
        a.advance_ready_epoch(was_a);
        assert_eq!(a.ready_epoch, 1);
        let key_a = a.ready_key().unwrap();
        // Observer B loads A's saved record: already ready, so its
        // observation is steady — no bump, the same persisted key.
        let mut b = a.clone();
        let was_b = b.merge_ready();
        assert!(was_b);
        b.observed = Some(green.clone());
        b.advance_ready_epoch(was_b);
        assert_eq!(b.ready_epoch, 1);
        assert_eq!(b.ready_key().as_deref(), Some(key_a.as_str()));
        // Post-lock wakes run B-before-A: identical keys, one message
        // id, one wake.
        assert_eq!(b.ready_key(), a.ready_key());
        // Regression then recovery is a new streak: red observes to no
        // key, green again bumps to a new one.
        let mut red = a.clone();
        red.observed = Some(Observed {
            ci_green: false,
            ..green.clone()
        });
        assert!(!red.merge_ready());
        assert_eq!(red.ready_key(), None);
        let was = red.merge_ready();
        red.observed = Some(green);
        red.advance_ready_epoch(was);
        assert_eq!(red.ready_epoch, 2);
        assert_ne!(red.ready_key(), a.ready_key());
        // A moved head never re-presents the old review: no key.
        red.observed = Some(Observed {
            head: "b".repeat(40),
            pr_state: "OPEN".into(),
            ci_green: true,
            ..Observed::default()
        });
        assert!(!red.merge_ready());
        assert_eq!(red.ready_key(), None);
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
