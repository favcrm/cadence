//! `cadence audit` — reconstruct every merge on the default branch
//! from data Cadence already stores, and flag the patterns the PM must
//! never have to hunt for by hand:
//!
//! - `reviewer==merger` — the same *GitHub* identity reviewed and
//!   merged (`qa-verdict` status `creator.login` vs `mergedBy.login`;
//!   note `From:` is an agent alias — a different namespace, rendered
//!   but never compared). When every GitHub identity in the run is ONE
//!   account (a shared token), the match is structural, not a finding:
//!   it is reported once in the summary and never flags a row (CAD-207).
//! - `no-passing-verdict` — no `pass` verdict bound to the exact head
//!   that landed (`qa-verdict` status plus the verdict note must agree
//!   on the squash-merged `headRefOid`).
//! - `approval-missing` / `approval-revoked` — a human-class merge with
//!   no operator approval record for the exact landed head that was in
//!   force at merge time (CAD-217). Records come from the daemon's
//!   approval stream (`cadence audit approve`); queue messages are
//!   never read as approvals or revocations. A bound record reports as
//!   `operator-claimed`, never verified, until CAD-280.
//!
//! Sources are read-only: merge commits on the default branch, `gh` PR
//! metadata and commit statuses, verdict/ops notes under the notes
//! directory, tracker folders, and the daemon's own event/verdict
//! tables (opened `SQLITE_OPEN_READ_ONLY`). Nothing is written at
//! merge time and nothing is written by this command — there is no
//! bookkeeping to keep in sync.
//!
//! Everything a source cannot prove is rendered `unknown` with the
//! reason — the audit never guesses provenance. And when a source
//! needed for the verdict question did not answer at all (gh down,
//! notes dir unreadable, store unopenable, reviewed head not in the
//! clone) the row is `evidence unavailable`, not an accusation: no
//! flag, no exit-1. `no-passing-verdict` is reserved for sources that
//! answered and said no.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{self, project};

mod approval;
mod delegated;
mod flags;
mod git;
mod github;
mod merge_content;
mod notes;
mod render;
mod store;
#[cfg(test)]
mod tests;
mod time;
mod tracker;

pub use self::approval::{approval_check, scope_check, verdicts_check};
pub(crate) use self::approval::{reviewer_identity, verdicts_in};
pub use self::delegated::digest;
pub use self::github::origin_slug;

use self::approval::*;
use self::delegated::*;
use self::flags::*;
use self::git::*;
use self::github::*;
use self::merge_content::*;
use self::notes::*;
use self::render::*;
use self::store::*;
use self::time::*;
use self::tracker::*;

/// Default notes directory — where verdict and ops-merge notes land.
const NOTES_DIR: &str = "/var/www/agent-notes";
/// Daemon store file under the state dir.
const STORE_FILE: &str = "cadence.sqlite3";
/// Cap on `gh` calls and on rows rendered by default (overridable with
/// `--limit`; pass `--limit 0` for no cap).
const DEFAULT_LIMIT: u64 = 200;
/// How far past a merge to look for post-merge evidence (restarts,
/// reverts, smoke runs) in events and ops notes.
const POST_MERGE_WINDOW_SECS: f64 = 48.0 * 3600.0;
/// Every subprocess the audit spawns is bounded — a wedged `gh` or a
/// pathological repo must not hang a digest.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const GH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub struct AuditOptions {
    /// `--since 24h|7d|YYYY-MM-DD|<epoch>` — drop merges older than this.
    pub since: Option<String>,
    /// `--class auto|notify|delegated|human` — keep rows of that class.
    pub class: Option<String>,
    /// `--project P` — keep rows whose tracker issue lives under project P.
    pub project: Option<String>,
    /// `--json` — one stable machine document (`cadence.audit/1`).
    pub json: bool,
    /// `--limit N` — cap rows (0 = all).
    pub limit: Option<u64>,
    /// Hidden: `--repo <path>` — audit this checkout instead of cwd.
    pub repo: Option<PathBuf>,
    /// Hidden: `--notes-dir <path>` — fixture the notes directory.
    pub notes_dir: Option<PathBuf>,
    /// Hidden: `--merge-report <path>` — fixture replacing every `gh`
    /// call (`{"prs":[…], "statuses":{"<sha>":{…}}}`).
    pub merge_report: Option<PathBuf>,
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
}

/// One reconstructed merge — every field is `Option`/flagged so the
/// JSON can carry `null` and the text can print `unknown` wherever a
/// source had no evidence.
#[derive(Debug, Default)]
struct Row {
    pr: Option<u64>,
    title: String,
    merge_sha: String,
    merged_at: Option<f64>,
    /// The squash-merged head (`headRefOid`) — what actually landed.
    landed_head: Option<String>,
    /// The head the verdict note bound its `pass` to.
    reviewed_head: Option<String>,
    /// `yes` | `no` | `unknown <reason>` — does the merge commit carry
    /// the reviewed head's change (ancestor for real merges, patch-id
    /// for squashes)?
    contains_head: String,
    /// `qa-verdict` commit status on the landed head: `SUCCESS`,
    /// `FAILURE`, `PENDING`, …
    qa_verdict_status: Option<String>,
    /// `creator.login` of whoever posted the `qa-verdict` status — the
    /// GitHub-namespace identity the `reviewer==merger` flag compares
    /// against `mergedBy.login`.
    qa_verdict_creator: Option<String>,
    /// The status post-dates the merge — post-hoc evidence, not a
    /// merge-time gate.
    status_post_hoc: bool,
    /// Channels that could not be read (gh down, notes dir missing,
    /// store unopenable, status fetch failed). Non-empty suppresses
    /// `no-passing-verdict` — the row is unknown, not accused.
    evidence_gaps: Vec<String>,
    /// Verdict recorded in a note or the verdicts table: `pass`,
    /// `fail`, `changes-requested`, …
    verdict: Option<String>,
    /// True when the bound verdict note's timestamp post-dates the
    /// merge — a post-hoc pass does not satisfy `no-passing-verdict`:
    /// at merge time nothing had reviewed the head that landed.
    verdict_post_hoc: bool,
    /// The verdict note's `From:` — an agent alias, display only. It
    /// is never compared to `merger`: the namespaces differ.
    reviewer: Option<String>,
    merger: Option<String>,
    class: Option<String>,
    trigger: Option<String>,
    /// Gate summary from the verdict note: raw lines plus compact
    /// suite/stress/flake fragments.
    gates: Vec<String>,
    gate_suite: Option<String>,
    gate_stress: Option<String>,
    gate_flakes: Option<String>,
    /// The reviewer's "what an auditor should check" line.
    auditor_check: Option<String>,
    /// Residue issue ids the verdict note filed (e.g. CAD-147).
    residue: Vec<String>,
    /// Tracker project key when the issue folder resolves.
    project: Option<String>,
    /// Issue ids named in the PR title (`CAD-92`, …).
    issues: Vec<String>,
    /// Post-merge outcome fields — each `yes|no|unknown <reason>`.
    tree_match: String,
    smoke: String,
    daemon_restart: String,
    revert: String,
    /// The repository's root commit — it predates any PR process and
    /// is exempt from the verdict flag.
    is_root: bool,
    /// Flag codes that fired on this row.
    flags: Vec<String>,
    /// Matches that hold on this row but cannot discriminate in this
    /// run (CAD-207) — e.g. `reviewer==merger` under one shared GitHub
    /// identity. Reported once in the summary, never flagged.
    structural: Vec<String>,
    /// Operator approval evidence bound to the landed head (CAD-217).
    approval: ApprovalView,
    /// CAD-918: a designated agent's delegated approval, bound the same
    /// way and reported apart — it never stands in for `approval`.
    delegated: ApprovalView,
    /// Native workflow consumption is separate from a per-head approval.
    workflow_authorization: Option<Value>,
    native_workflow_review: bool,
    /// `(field, reason)` pairs for every `unknown` rendered.
    unknowns: Vec<(String, String)>,
}

pub fn run(opts: &AuditOptions) -> Result<i32> {
    let repo = opts.repo.clone().unwrap_or_else(|| opts.cwd.clone());
    let repo = repo.canonicalize().unwrap_or(repo);
    if git(&repo, &["rev-parse".into(), "--git-dir".into()]).is_err() {
        return Err(Error::rejected(format!(
            "{} is not a git repository",
            repo.display()
        )));
    }
    let class_filter = match opts.class.as_deref() {
        Some(c) => {
            let c = c.to_lowercase();
            if !["auto", "notify", "delegated", "human"].contains(&c.as_str()) {
                return Err(Error::rejected(format!(
                    "--class must be auto, notify, delegated or human (got '{c}')"
                )));
            }
            Some(c)
        }
        None => None,
    };
    let since = opts
        .since
        .as_deref()
        .map(parse_since)
        .transpose()?
        .unwrap_or(0.0);
    let notes_dir = opts
        .notes_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(NOTES_DIR));

    // 1. Merge commits on the default branch — the authoritative list.
    let default_ref = default_ref(&repo)?;
    let merges = merge_commits(&repo, &default_ref)?;
    // One log scan for revert evidence, shared by every row.
    let branch_log = branch_log(&repo, &default_ref);

    // 2. GitHub enrichment (or the fixture): one `pr list` plus one
    //    status call per landed head.
    let mut gh = match &opts.merge_report {
        Some(path) => merge_fixture(path)?,
        None => github(&repo)?,
    };

    // 3. Notes index — one scan; match notes to merges by SHA mention.
    //    `None` when the directory itself cannot be read: every row's
    //    verdict evidence is then unavailable, not absent.
    let notes = note_index(&notes_dir);

    // 4. Daemon store, opened read-only — events + the verdicts table.
    let store = store_evidence(&opts.state_dir.join(STORE_FILE), true);

    // 5. Tracker: issue id → project folder, for --project scoping.
    let pm = issue::default_dir().ok();

    // Pass 1 — local/cached sources only (the pr list is one bulk call,
    // already fetched): build the row, filter, cap. Per-row `gh api`
    // status calls happen in pass 2 so `--limit` actually bounds them.
    let mut rows = Vec::new();
    for (merge_sha, at, parents, subject) in merges {
        if (at as f64) < since {
            continue;
        }
        let mut row = row_from_subject(&merge_sha, at, parents, &subject);
        enrich_gh_pr(&gh, &mut row);
        enrich_notes(notes.as_deref(), &notes_dir, &mut row);
        enrich_store(&store, &opts.state_dir.join(STORE_FILE), &mut row);
        enrich_tracker(pm.as_deref(), &mut row);
        if class_filter
            .as_deref()
            .is_some_and(|c| row.class.as_deref() != Some(c))
        {
            continue;
        }
        if let Some(p) = &opts.project {
            if row.project.as_deref() != Some(p.as_str()) {
                continue;
            }
        }
        rows.push(row);
    }
    // `--limit` caps in-window rows (newest-first) — applied after the
    // since/class/project filters so an early cutoff can't hide them,
    // and *before* any per-row `gh api` call so it bounds the fan-out.
    let limit = opts.limit.unwrap_or(DEFAULT_LIMIT);
    if limit > 0 {
        rows.truncate(limit as usize);
    }

    // Pass 2 — the rows that render get their per-head evidence:
    // qa-verdict status (one `gh api` call each), merge-content check,
    // post-merge outcomes, the approval binding, then flags.
    // The repo an approval must be scoped to — gh's slug, or the
    // checkout's own origin in fixture runs.
    let approval_slug = gh.slug.clone().or_else(|| origin_slug(&repo));
    for row in &mut rows {
        enrich_status(&mut gh, row);
        contains_head(&repo, row);
        post_merge(&branch_log, &store, row);
    }
    enrich_workflow_authorizations(&store, approval_slug.as_deref(), &mut rows);
    for row in &mut rows {
        enrich_approval(&store, approval_slug.as_deref(), row);
        finalize_unknowns(row);
        flag_row(row);
    }
    // Fleet-level pass (CAD-207): a match every row shares is a fact
    // about the fleet, reported once — not a per-row finding. Decided
    // over every merged PR gh returned, not just the rendered window —
    // and not at all while any of their statuses is out of hand (CAD-287).
    let structural = fleet_logins(&gh).and_then(|fleet| apply_structural(&mut rows, &fleet));

    let flagged = rows.iter().filter(|r| !r.flags.is_empty()).count();
    if opts.json {
        print_json(
            &repo,
            &default_ref,
            &rows,
            flagged,
            structural.as_ref(),
            opts,
            since,
        );
    } else {
        print!(
            "{}",
            render_text(
                &repo,
                &default_ref,
                &rows,
                flagged,
                structural.as_ref(),
                opts
            )
        );
    }
    Ok(if flagged > 0 { 1 } else { 0 })
}
