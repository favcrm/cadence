//! `cadence audit` — reconstruct every merge on the default branch
//! from data Cadence already stores, and flag the patterns the PM must
//! never have to hunt for by hand:
//!
//! - `reviewer==merger` — the same *GitHub* identity reviewed and
//!   merged (`qa-verdict` status `creator.login` vs `mergedBy.login`;
//!   note `From:` is an agent alias — a different namespace, rendered
//!   but never compared).
//! - `no-passing-verdict` — no `pass` verdict bound to the exact head
//!   that landed (`qa-verdict` status plus the verdict note must agree
//!   on the squash-merged `headRefOid`).
//! - `approval-missing` / `approval-revoked` — a human-class merge with
//!   no operator approval record for the exact landed head that was in
//!   force at merge time (CAD-217). Records come from the daemon's
//!   approval stream (`cadence audit approve`); queue messages are
//!   never read as approvals or revocations.
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
    /// `--class auto|notify|human` — keep rows classified to that class.
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
    /// Operator approval evidence bound to the landed head (CAD-217).
    approval: ApprovalView,
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
            if !["auto", "notify", "human"].contains(&c.as_str()) {
                return Err(Error::rejected(format!(
                    "--class must be auto, notify or human (got '{c}')"
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
    let gh = match &opts.merge_report {
        Some(path) => merge_fixture(path)?,
        None => github(&repo)?,
    };

    // 3. Notes index — one scan; match notes to merges by SHA mention.
    //    `None` when the directory itself cannot be read: every row's
    //    verdict evidence is then unavailable, not absent.
    let notes = note_index(&notes_dir);

    // 4. Daemon store, opened read-only — events + the verdicts table.
    let store = store_evidence(&opts.state_dir.join(STORE_FILE));

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
    for row in &mut rows {
        enrich_status(gh.slug.as_deref(), &gh, row);
        contains_head(&repo, row);
        post_merge(&branch_log, &store, row);
        enrich_approval(&store, gh.slug.as_deref(), row);
        finalize_unknowns(row);
        flag_row(row);
    }

    let flagged = rows.iter().filter(|r| !r.flags.is_empty()).count();
    if opts.json {
        print_json(&repo, &default_ref, &rows, flagged, opts, since);
    } else {
        print!("{}", render_text(&repo, &default_ref, &rows, flagged, opts));
    }
    Ok(if flagged > 0 { 1 } else { 0 })
}

// ---------- git ------------------------------------------------------

/// Every subprocess runs under `run_bounded` — a hung `git` or `gh`
/// cannot stall the audit.
fn git(repo: &Path, args: &[String]) -> std::result::Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    let out = crate::proc::run_bounded(&mut cmd, GIT_TIMEOUT)
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `origin/HEAD` symref, else origin/main, origin/master, then local.
fn default_ref(repo: &Path) -> Result<String> {
    if let Ok(sym) = git(
        repo,
        &["symbolic-ref".into(), "refs/remotes/origin/HEAD".into()],
    ) {
        let sym = sym.trim().to_string();
        if let Some(short) = sym.strip_prefix("refs/remotes/") {
            return Ok(short.to_string());
        }
    }
    for cand in ["origin/main", "origin/master", "main", "master"] {
        if git(repo, &["rev-parse".into(), "--verify".into(), cand.into()]).is_ok() {
            return Ok(cand.to_string());
        }
    }
    Err(Error::rejected(
        "no default branch found (origin/HEAD, origin/main, origin/master, main, master)",
    ))
}

/// `sha \x1f committer-epoch \x1f parent-count \x1f subject` per
/// first-parent commit on the ref — a real merge yields its merge
/// commit, not one row per side-branch commit.
fn merge_commits(repo: &Path, reference: &str) -> Result<Vec<(String, i64, usize, String)>> {
    let out = git(
        repo,
        &[
            "log".into(),
            "--first-parent".into(),
            reference.into(),
            "--format=%H%x1f%ct%x1f%P%x1f%s".into(),
        ],
    )
    .map_err(Error::rejected)?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\x1f');
            Some((
                p.next()?.to_string(),
                p.next()?.parse().ok()?,
                p.next()?.split(' ').filter(|s| !s.is_empty()).count(),
                p.next().unwrap_or("").to_string(),
            ))
        })
        .collect())
}

/// The PR number from a merge subject: `Merge pull request #N …` for
/// real merges, `(#N)` anywhere for squashes (the last occurrence — a
/// trailing ` (rebased)` marker does not hide it).
fn pr_number(subject: &str) -> Option<u64> {
    if let Some(rest) = subject.trim_start().strip_prefix("Merge pull request #") {
        return rest
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok();
    }
    subject
        .rmatch_indices("(#")
        .filter_map(|(i, _)| {
            let tail = &subject[i + 2..];
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            (tail.as_bytes().get(digits.len()) == Some(&b')')).then(|| digits.parse().ok())?
        })
        .next()
}

/// `CAD-NN` issue ids mentioned anywhere in a subject.
fn issue_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices("CAD-") {
        let mut end = i + 4;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > i + 4 {
            let id = text[i..end].to_string();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

fn row_from_subject(merge_sha: &str, at: i64, parents: usize, subject: &str) -> Row {
    let pr = pr_number(subject);
    let title = match (pr, subject.rfind(" (#")) {
        (Some(_), Some(i)) => subject[..i].to_string(),
        _ => subject.to_string(),
    };
    let mut row = Row {
        pr,
        is_root: parents == 0,
        title,
        merge_sha: merge_sha.to_string(),
        merged_at: Some(at as f64),
        issues: issue_ids(subject),
        contains_head: "unknown (not checked)".into(),
        tree_match: "unknown (not checked)".into(),
        smoke: "unknown (no smoke record)".into(),
        daemon_restart: "unknown (no restart record)".into(),
        revert: "no".into(),
        ..Default::default()
    };
    if row.pr.is_none() {
        row.unknowns
            .push(("pr".into(), "subject has no (#NN) suffix".into()));
    }
    row
}

// ---------- github ---------------------------------------------------

/// The `gh` payload: merged-PR rows plus per-sha commit statuses.
#[derive(Default)]
struct Gh {
    prs: HashMap<u64, Value>,
    statuses: HashMap<String, Value>,
    error: Option<String>,
    /// `owner/repo` — resolved once at startup; per-row status calls
    /// reuse it instead of re-running `git remote get-url` per head.
    slug: Option<String>,
    /// `--merge-report` set: every `gh` lookup resolves from the
    /// fixture — a miss is `unknown`, never a live call.
    fixture: bool,
}

/// `owner/name` of the checkout's github.com `origin` — the scope an
/// approval record binds to (also used by `cadence audit approve`).
pub fn origin_slug(repo: &Path) -> Option<String> {
    let url = git(repo, &["remote".into(), "get-url".into(), "origin".into()]).ok()?;
    let norm = project::normalize_remote(url.trim());
    norm.strip_prefix("github.com/").map(str::to_string)
}

fn github(repo: &Path) -> Result<Gh> {
    let Some(slug) = origin_slug(repo) else {
        return Ok(Gh {
            error: Some("no github.com origin remote".into()),
            ..Default::default()
        });
    };
    let mut gh = Gh {
        slug: Some(slug.clone()),
        ..Default::default()
    };
    let prs = gh_text(&[
        "pr".into(),
        "list".into(),
        "--repo".into(),
        slug.clone(),
        "--state".into(),
        "merged".into(),
        // One page covers far more than any sane audit window — a PR
        // absent from this list is a data gap, not a truncation.
        "--limit".into(),
        "1000".into(),
        "--json".into(),
        "number,title,mergedBy,mergeCommit,headRefOid,mergedAt".into(),
    ]);
    match prs.and_then(|t| {
        serde_json::from_str::<Value>(&t).map_err(|e| format!("gh pr list: unreadable ({e})"))
    }) {
        Ok(Value::Array(list)) => {
            for pr in list {
                if let Some(n) = pr["number"].as_u64() {
                    gh.prs.insert(n, pr);
                }
            }
        }
        Ok(_) => gh.error = Some("gh pr list: not a JSON array".into()),
        Err(e) => gh.error = Some(e),
    }
    Ok(gh)
}

/// Load a `--merge-report` fixture: `{"prs":[…], "statuses":{"<sha>":{…}}}`.
/// A bare array is treated as the `prs` list. Errors are hard — a bad
/// fixture must never fall back to live `gh`.
fn merge_fixture(path: &Path) -> Result<Gh> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::rejected(format!("--merge-report {}: {e}", path.display())))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| {
        Error::rejected(format!("--merge-report {}: bad JSON ({e})", path.display()))
    })?;
    let mut gh = Gh::default();
    let prs = if v.is_array() { &v } else { &v["prs"] };
    for pr in prs.as_array().cloned().unwrap_or_default() {
        if let Some(n) = pr["number"].as_u64() {
            gh.prs.insert(n, pr);
        }
    }
    for (sha, st) in v["statuses"].as_object().cloned().unwrap_or_default() {
        gh.statuses.insert(sha, st);
    }
    gh.fixture = true;
    Ok(gh)
}

fn gh_text(args: &[String]) -> std::result::Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(args);
    let out = crate::proc::run_bounded(&mut cmd, GH_TIMEOUT).map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The `qa-verdict` context on one commit status payload: its state,
/// the GitHub `creator.login` that posted it, and `created_at`. Status
/// contexts report `success`, check runs `SUCCESS` — normalized to the
/// uppercase StatusCheckRollup spelling.
fn qa_verdict_state(status: &Value) -> Option<(String, Option<String>, Option<f64>)> {
    for ctx in status["statuses"].as_array().into_iter().flatten() {
        if ctx["context"].as_str() == Some("qa-verdict") {
            let Some(state) = ctx["state"].as_str() else {
                continue;
            };
            let at = ctx["created_at"].as_str().and_then(parse_iso);
            return Some((
                state.to_uppercase(),
                ctx["creator"]["login"].as_str().map(str::to_string),
                at.map(|t| t as f64),
            ));
        }
    }
    // Check runs arrive under `check_runs` on some endpoints.
    for run in status["check_runs"].as_array().into_iter().flatten() {
        if run["name"].as_str() == Some("qa-verdict") {
            let Some(state) = run["conclusion"]
                .as_str()
                .or_else(|| run["status"].as_str())
            else {
                continue;
            };
            let at = run["started_at"]
                .as_str()
                .or_else(|| run["completed_at"].as_str())
                .and_then(parse_iso);
            return Some((
                state.to_uppercase(),
                run["user"]["login"]
                    .as_str()
                    .or_else(|| run["app"]["slug"].as_str())
                    .map(str::to_string),
                at.map(|t| t as f64),
            ));
        }
    }
    None
}

/// PR metadata from the one `gh pr list` call — merger, landed head,
/// mergedAt, title. No network here; the per-sha status fetch happens
/// post-filter in `enrich_status`.
fn enrich_gh_pr(gh: &Gh, row: &mut Row) {
    let Some(n) = row.pr else { return };
    let Some(pr) = gh.prs.get(&n) else {
        let reason = gh
            .error
            .as_ref()
            .map(|e| format!("gh unavailable: {e}"))
            .unwrap_or_else(|| format!("no merged PR #{n} in gh pr list"));
        row.unknowns.push(("merged_by".into(), reason.clone()));
        row.evidence_gaps.push(reason);
        return;
    };
    row.title = pr["title"].as_str().unwrap_or(&row.title).to_string();
    row.merger = pr["mergedBy"]["login"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    row.landed_head = pr["headRefOid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // gh's mergeCommit must be the commit git log shows — a mismatch
    // means the PR number in the subject points at a different merge.
    if let Some(mc) = pr["mergeCommit"]["oid"].as_str() {
        if mc != row.merge_sha {
            row.unknowns.push((
                "merge_sha".into(),
                format!("gh mergeCommit {mc} != git log {}", row.merge_sha),
            ));
        }
    }
    if let Some(m) = pr["mergedAt"].as_str() {
        row.merged_at = parse_iso(m).map(|t| t as f64).or(row.merged_at);
    }
    if row.merger.is_none() {
        row.unknowns
            .push(("merged_by".into(), "gh reports no mergedBy".into()));
    }
    if row.landed_head.is_none() {
        row.unknowns
            .push(("landed_head".into(), "gh reports no headRefOid".into()));
        // No head → the status channel cannot be queried.
        row.evidence_gaps.push("gh reports no headRefOid".into());
    }
}

/// The per-row network call — `qa-verdict` status on the exact landed
/// head. Runs only for rows that survive the filters, so a narrow
/// `--since`/`--class`/`--limit` audit spends one API call per shown
/// merge, not per in-window commit.
fn enrich_status(slug: Option<&str>, gh: &Gh, row: &mut Row) {
    // Direct pushes have no PR head — but the merge commit itself can
    // still carry a `qa-verdict` status, so it is queried too.
    let head = row
        .landed_head
        .clone()
        .unwrap_or_else(|| row.merge_sha.clone());
    if row.pr.is_some() && row.landed_head.is_none() {
        return; // gap already recorded by enrich_gh_pr
    }
    let status = match gh.statuses.get(head.as_str()) {
        Some(s) => Some(Ok(s.clone())),
        // Fixture mode never falls back to a live call; a missing key
        // is the fixture saying "no statuses on this head".
        None if gh.fixture => None,
        None => slug.map(|s| gh_status(s, head.as_str())),
    };
    match status {
        Some(Err(e)) => {
            row.evidence_gaps.push(format!("status fetch failed: {e}"));
            row.unknowns.push((
                "qa_verdict_status".into(),
                format!("status fetch failed on {head}: {e}"),
            ));
        }
        Some(Ok(v)) => match qa_verdict_state(&v) {
            Some((state, creator, at)) => {
                row.qa_verdict_status = Some(state);
                row.qa_verdict_creator = creator;
                row.status_post_hoc = match (at, row.merged_at) {
                    (Some(at), Some(m)) => at > m,
                    _ => false,
                };
            }
            None => row.unknowns.push((
                "qa_verdict_status".into(),
                format!("no qa-verdict status on {head}"),
            )),
        },
        None => {}
    }
}

/// The `qa-verdict` evidence for one head. Two endpoints, in order:
/// `statuses/{ref}` is the only one that returns `creator.login` (the
/// combined `/status` payload omits it), and the combined endpoint is
/// the only one that reports check runs — the fallback when a
/// qa-verdict was posted as a check run instead of a status.
fn gh_status(slug: &str, sha: &str) -> std::result::Result<Value, String> {
    let list = gh_text(&[
        "api".into(),
        format!("repos/{slug}/statuses/{sha}?per_page=100"),
    ])?;
    let list: Value =
        serde_json::from_str(&list).map_err(|e| format!("gh api statuses: unreadable ({e})"))?;
    let wrapped = json!({"statuses": list});
    if qa_verdict_state(&wrapped).is_some() {
        return Ok(wrapped);
    }
    let combined = gh_text(&[
        "api".into(),
        format!("repos/{slug}/commits/{sha}/status?per_page=100"),
    ])?;
    let combined: Value =
        serde_json::from_str(&combined).map_err(|e| format!("gh api status: unreadable ({e})"))?;
    if qa_verdict_state(&combined).is_some() {
        return Ok(combined);
    }
    // Both endpoints answered and neither reports a qa-verdict — that
    // is an answer, not a gap.
    Ok(wrapped)
}

// ---------- notes ----------------------------------------------------

/// One note's extracted evidence. Only the fields the audit renders.
#[derive(Debug, Default)]
struct Note {
    path: PathBuf,
    /// `verdict` | `ops-merge` | `other` — from the filename.
    kind: String,
    /// `From:` identity.
    from: Option<String>,
    /// Full 40-hex tokens seen in the note.
    shas: Vec<String>,
    /// `#NN` PR references seen in the note.
    prs: Vec<u64>,
    /// The `Issue:` header — binds a verdict to one tracker issue.
    issue: Option<String>,
    /// The sha a `head …` line names — the verdict's reviewed head.
    head_sha: Option<String>,
    verdict: Option<String>,
    class: Option<String>,
    trigger: Option<String>,
    auditor_check: Option<String>,
    gates: Vec<String>,
    residue: Vec<String>,
    /// Post-merge outcome lines (`tree`, `smoke`, `restart`, `revert`).
    outcomes: HashMap<String, String>,
    /// Compact gate summary: suite result, stress runs, disclosed flakes.
    gate_suite: Option<String>,
    gate_stress: Option<String>,
    gate_flakes: Option<String>,
}

/// One compact gate fragment: `N passed/M failed` when the line carries
/// counts, else the line trimmed of markdown and truncated at 80 chars.
fn gate_fragment(line: &str) -> String {
    let l = line.trim_start_matches(['-', '*', ' ']).trim();
    let count = |key: &str| -> Option<String> {
        let i = l.find(key)?;
        let num: String = l[..i]
            .trim_end()
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        (!num.is_empty()).then_some(num)
    };
    match (count("passed"), count("failed")) {
        (Some(p), Some(f)) => return format!("{p} passed/{f} failed"),
        (Some(p), None) => return format!("{p} passed"),
        _ => {}
    }
    // `223/224` style tallies.
    let b = l.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'/' && i > 0 {
            let a: String = l[..i]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let n: String = l[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !a.is_empty() && !n.is_empty() {
                return format!("{a}/{n}");
            }
        }
    }
    // No tally — the leading clause is the summary; the full line
    // stays in `gates` either way.
    let lead = l
        .split(['.', ';'])
        .next()
        .unwrap_or(l)
        .trim_matches('`')
        .trim();
    lead.chars().take(60).collect()
}

/// A gate line records stress runs when it says `stress` or carries a
/// repeat count (`×3`, `x5`, `3x`).
fn gate_is_stress(lower: &str) -> bool {
    if lower.contains("stress") {
        return true;
    }
    let b = lower.as_bytes();
    for i in 0..b.len().saturating_sub(1) {
        let rep = (b[i] == b'x' || b[i] == 0xC3 && i + 1 < b.len() && b[i + 1] == 0x97)
            && (b.get(i + 1).is_some_and(|c| c.is_ascii_digit())
                || b.get(i + 2).is_some_and(|c| c.is_ascii_digit())
                || i > 0 && b[i - 1].is_ascii_digit());
        if rep {
            return true;
        }
    }
    false
}

/// `None` when the notes directory itself cannot be read — distinct
/// from an empty one: an unreadable directory is missing evidence, an
/// empty one is an answered "no verdicts".
fn note_index(dir: &Path) -> Option<Vec<Note>> {
    let mut notes = Vec::new();
    let read = std::fs::read_dir(dir).ok()?;
    for ent in read.flatten() {
        let path = ent.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        notes.push(parse_note(&path, name, &text));
    }
    Some(notes)
}

fn parse_note(path: &Path, name: &str, text: &str) -> Note {
    let kind = if name.contains("-verdict") {
        "verdict"
    } else if name.contains("merge") {
        "ops-merge"
    } else {
        "other"
    }
    .to_string();
    let mut note = Note {
        path: path.to_path_buf(),
        kind,
        shas: hex_shas(text),
        prs: pr_refs(text),
        ..Default::default()
    };
    let mut in_gates = false;
    for line in text.lines() {
        let t = line.trim();
        let lower = t.to_lowercase();
        if let Some(rest) = t
            .strip_prefix("From:")
            .or_else(|| t.strip_prefix("> From:"))
        {
            note.from = Some(rest.trim().trim_matches('`').to_string());
        }
        if note.issue.is_none() {
            if let Some(rest) = t
                .strip_prefix("Issue:")
                .or_else(|| t.strip_prefix("> Issue:"))
            {
                note.issue = issue_ids(rest).into_iter().next();
            }
        }
        // `head <sha>` names the reviewed head — it may sit mid-line
        // (`pass — head `abc123``), so scan the text after `head`.
        // All-digit tokens stay in this context (`5428215` is a real
        // abbrev; a date would not sit on a head line).
        if note.head_sha.is_none() {
            // `head` as a word — `ahead`/`overhead` don't count; the
            // sha follows a separator (`head 5428215`, `head: `abc``,
            // `head=…`). Scan `t` directly with char-boundary-safe
            // indexing — a lowercase offset cannot index `t`: `ẞ`/`İ`
            // change byte length under `to_lowercase`.
            for (i, c) in t.char_indices() {
                if !c.eq_ignore_ascii_case(&'h') {
                    continue;
                }
                let Some(seg) = t.get(i..i + 4) else { continue };
                if !seg.eq_ignore_ascii_case("head") {
                    continue;
                }
                let bounded = t[..i]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_ascii_alphabetic())
                    && t.get(i + 4..)
                        .and_then(|s| s.chars().next())
                        .is_none_or(|c| !c.is_ascii_alphanumeric());
                if bounded {
                    note.head_sha = t
                        .get(i + 4..)
                        .and_then(|s| hex_shas_ctx(s, true).into_iter().next());
                    break;
                }
            }
        }
        if let Some(rest) = t.to_lowercase().strip_prefix("verdict:") {
            note.verdict = Some(
                rest.trim()
                    .trim_matches('`')
                    .split(' ')
                    .next()
                    .unwrap_or("")
                    .to_string(),
            );
        }
        if lower.starts_with("risk:") || lower.starts_with("**risk:") {
            let rest = t
                .trim_start_matches('*')
                .trim_start_matches("Risk:")
                .trim_start_matches("risk:")
                .trim_end_matches('*')
                .trim();
            let (class, trigger) = match rest.split_once(' ') {
                Some((c, tr)) => {
                    // `class (trigger) prose` — keep the parenthesized
                    // trigger; unparenthesized prose caps at the first
                    // sentence.
                    let trig = tr
                        .strip_prefix('(')
                        .and_then(|s| s.split(')').next())
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            let t = tr.split(". ").next().unwrap_or(tr);
                            t.chars().take(140).collect()
                        });
                    (c.to_string(), trig)
                }
                None => (rest.to_string(), String::new()),
            };
            let class = class.trim_end_matches('*').to_lowercase();
            if ["auto", "notify", "human"].contains(&class.as_str()) {
                note.class = Some(class);
                if !trigger.is_empty() {
                    note.trigger = Some(trigger);
                }
            }
        }
        if lower.contains("auditor should check") {
            // The label is boilerplate ("What an auditor should check
            // after the merge:**") — keep only the check itself.
            let body = t
                .split_once(':')
                .map(|(_, b)| b)
                .unwrap_or(t)
                .trim_start_matches(['*', ' '])
                .trim_end_matches('*')
                .to_string();
            note.auditor_check = Some(body);
        }
        if lower.starts_with("## gates")
            || lower.starts_with("gates —")
            || lower.starts_with("gates:")
        {
            in_gates = true;
        } else if t.starts_with("## ") {
            in_gates = false;
        }
        // Older verdicts keep gates under `## Findings` — catch the
        // gate lines themselves (`Gates on…`, `Full integration suite`).
        let gate_line = in_gates
            || lower.starts_with("gates")
            || lower.contains("integration suite")
            || lower.starts_with("- full integration");
        if gate_line && !lower.starts_with("## gates") && !t.is_empty() && !t.starts_with("#") {
            note.gates.push(t.to_string());
            // Compact fields: suite result, stress runs, flakes.
            if note.gate_suite.is_none()
                && (lower.contains("integration")
                    || lower.contains("suite")
                    || lower.contains("cargo test"))
            {
                note.gate_suite = Some(gate_fragment(t));
            }
            if note.gate_stress.is_none() && gate_is_stress(&lower) {
                note.gate_stress = Some(gate_fragment(t));
            }
            if lower.contains("flake") || lower.contains("disclos") {
                let ids = issue_ids(t);
                note.gate_flakes = Some(if ids.is_empty() {
                    gate_fragment(t)
                } else {
                    ids.join(" ")
                });
            }
        }
        // Residue: `CAD-NN` ids on lines mentioning residue/follow-up.
        if lower.contains("residue") || lower.contains("follow-up") {
            for id in issue_ids(t) {
                if !note.residue.contains(&id) {
                    note.residue.push(id);
                }
            }
        }
        // Post-merge outcome lines — ops-merge notes carry smoke /
        // daemon restart records; verdict notes carry post-hoc
        // tree checks (`range-diff empty`, `tree equals`). `revert`
        // is never taken from prose — the git log is authoritative.
        if !t.starts_with('#') {
            let is_tree = lower.contains("tree match")
                || lower.contains("tree-match")
                || lower.contains("range-diff")
                || (lower.contains("tree") && lower.contains("equal"))
                || (lower.contains("patch") && lower.contains("identical"));
            if is_tree {
                note.outcomes
                    .entry("tree_match".into())
                    .or_insert_with(|| t.to_string());
            }
            for (key, field) in [("smoke", "smoke"), ("daemon restart", "daemon_restart")] {
                if lower.contains(key) {
                    note.outcomes
                        .entry(field.into())
                        .or_insert_with(|| t.to_string());
                }
            }
        }
    }
    // `## Verdict` section fallback — first non-empty line is the verdict.
    if note.verdict.is_none() {
        let mut seen = false;
        for line in text.lines() {
            let l = line.trim();
            if l.to_lowercase().starts_with("## verdict") {
                seen = true;
                continue;
            }
            if seen && !l.is_empty() {
                note.verdict = Some(
                    l.trim_matches('`')
                        .split([' ', '—', '-'])
                        .next()
                        .unwrap_or("")
                        .to_string(),
                );
                break;
            }
        }
    }
    note
}

/// Hex tokens ≥7 chars in a blob. In general context all-digit tokens
/// are excluded — note filenames carry `YYYYMMDD` stamps that are not
/// SHAs — but all-letter tokens stay: `cabbebe` is a real abbrev. In a
/// `head …` context (`head_sha`) all-digit tokens stay too: a real
/// abbrev can be all digits (`5428215`).
fn hex_shas(text: &str) -> Vec<String> {
    hex_shas_ctx(text, false)
}

fn hex_shas_ctx(text: &str, head_ctx: bool) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !(c.is_ascii_hexdigit())) {
        // General context requires an a-f letter: `YYYYMMDD` date
        // stamps must not parse as SHAs, but all-letter abbrevs
        // (`cabbebe`) are real. A `head …` context accepts any all-hex
        // token — all-digit abbrevs (`5428215`) are real there too.
        let ok = (7..=40).contains(&word.len())
            && (head_ctx || word.chars().any(|c| ('a'..='f').contains(&c)));
        if ok {
            out.push(word.to_string());
        }
    }
    out
}

/// `#NN` PR references in a blob (`PR #54`, `(#54)`).
fn pr_refs(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'#' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                if let Ok(n) = text[i + 1..j].parse::<u64>() {
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
            }
        }
    }
    out
}

fn sha_hit(note: &Note, sha: &str) -> bool {
    sha.len() >= 7
        && note
            .shas
            .iter()
            .any(|s| s.len() >= 7 && (s.starts_with(sha) || sha.starts_with(s.as_str())))
}

/// A verdict note is evidence for a merge when the head it declares
/// (`head <sha>`) is the landed head — or, with no head line, when it
/// names the landed head at all. A note naming the merge but a
/// different head is the stale-verdict case the audit flags, not a pass.
fn verdict_matches(note: &Note, row: &Row) -> bool {
    let landed = row.landed_head.as_deref().unwrap_or(&row.merge_sha);
    match &note.head_sha {
        Some(h) => {
            h.len() >= 7
                && (landed.starts_with(h) || h.starts_with(&landed[..landed.len().min(h.len())]))
        }
        None => sha_hit(note, landed),
    }
}

/// A verdict note *for this issue* that declares a different head —
/// evidence of a verdict on a head that never landed. The `Issue:`
/// header must match so a note that merely mentions the PR in passing
/// does not borrow another merge's verdict.
fn stale_verdict<'a>(notes: &'a [Note], row: &Row) -> Option<&'a Note> {
    notes
        .iter()
        .filter(|n| n.kind == "verdict" && n.head_sha.is_some() && !verdict_matches(n, row))
        .filter(|n| {
            n.issue.as_ref().is_some_and(|i| row.issues.contains(i))
                || (n.issue.is_none()
                    && (sha_hit(n, &row.merge_sha) || row.pr.is_some_and(|p| n.prs.contains(&p))))
        })
        .max_by_key(|n| n.path.clone())
}

/// Ops-merge notes bind by merge commit or landed head.
fn ops_matches(note: &Note, row: &Row) -> bool {
    sha_hit(note, &row.merge_sha) || row.landed_head.as_deref().is_some_and(|h| sha_hit(note, h))
}

fn enrich_notes(notes: Option<&[Note]>, dir: &Path, row: &mut Row) {
    let Some(notes) = notes else {
        let reason = format!("notes dir {} unreadable", dir.display());
        row.evidence_gaps.push(reason.clone());
        row.unknowns.push(("verdict".into(), reason));
        return;
    };
    let verdict = notes
        .iter()
        .filter(|n| n.kind == "verdict" && verdict_matches(n, row))
        .max_by_key(|n| n.path.clone());
    if let Some(n) = verdict {
        row.reviewer = n.from.clone();
        if row.verdict.is_none() {
            row.verdict = n.verdict.clone();
        }
        row.class = n.class.clone().or(row.class.take());
        row.trigger = n.trigger.clone().or(row.trigger.take());
        row.auditor_check = n.auditor_check.clone().or(row.auditor_check.take());
        row.gates = n.gates.clone();
        row.gate_suite = n.gate_suite.clone();
        row.gate_stress = n.gate_stress.clone();
        row.gate_flakes = n.gate_flakes.clone();
        row.residue = n.residue.clone();
        row.reviewed_head = n.head_sha.clone().or_else(|| row.landed_head.clone());
        // Filename `YYYYMMDD-HHMMSS` vs merge time → post-hoc verdict.
        if let (Some(at), Some(name)) = (row.merged_at, n.path.file_name().and_then(|s| s.to_str()))
        {
            if let Some(ts) =
                crate::issue::time::note_name_to_iso(name).and_then(|iso| parse_iso(&iso))
            {
                row.verdict_post_hoc = (ts as f64) > at;
            }
        }
    } else if let Some(stale) = stale_verdict(notes, row) {
        // A verdict exists but names a head that never landed — show
        // it as the reviewed head so the divergence is visible.
        row.reviewed_head = stale.head_sha.clone();
        row.unknowns.push((
            "verdict".into(),
            format!(
                "verdict note {} names head {} — not the landed head",
                stale.path.display(),
                stale.head_sha.as_deref().unwrap_or("?")
            ),
        ));
    }
    // Ops-merge notes carry every outcome field; a bound verdict
    // note's post-hoc check contributes tree_match only.
    let outcomes: Vec<(String, String)> = notes
        .iter()
        .filter(|n| {
            (n.kind == "ops-merge" && ops_matches(n, row))
                || (n.kind == "verdict" && verdict_matches(n, row))
        })
        .flat_map(|n| {
            n.outcomes
                .iter()
                .filter(|(k, _)| n.kind == "ops-merge" || k.as_str() == "tree_match")
                .map(|(k, v)| (k.clone(), v.clone()))
        })
        .collect();
    for (k, v) in outcomes {
        let slot = match k.as_str() {
            "tree_match" => &mut row.tree_match,
            "smoke" => &mut row.smoke,
            "daemon_restart" => &mut row.daemon_restart,
            _ => continue,
        };
        if slot.starts_with("unknown") {
            *slot = v;
        }
    }
}

/// After every source has run: any field still unset that no source
/// explained gets a generic `unknown` reason — the row never renders
/// `unknown` without saying why.
fn finalize_unknowns(row: &mut Row) {
    let mut add = |field: &str, none: bool, reason: &str| {
        if none && !row.unknowns.iter().any(|(f, _)| f == field) {
            row.unknowns.push((field.into(), reason.into()));
        }
    };
    add(
        "reviewer",
        row.reviewer.is_none(),
        "no verdict note names this head",
    );
    add(
        "qa_verdict_creator",
        row.qa_verdict_status.is_some() && row.qa_verdict_creator.is_none(),
        "the qa-verdict status records no creator.login",
    );
    add("merger", row.merger.is_none(), "no source records a merger");
    add(
        "verdict",
        row.verdict.is_none(),
        "no verdict note or store row for this head",
    );
    add(
        "reviewed_head",
        row.reviewed_head.is_none(),
        "no verdict note names a reviewed head",
    );
    add(
        "landed_head",
        row.landed_head.is_none(),
        "gh reports no headRefOid",
    );
    add(
        "qa_verdict_status",
        row.qa_verdict_status.is_none(),
        "no qa-verdict status on the head",
    );
    add(
        "class",
        row.class.is_none(),
        "no verdict note records a risk class",
    );
    add(
        "auditor_check",
        row.auditor_check.is_none(),
        "no 'what an auditor should check' line recorded",
    );
}

// ---------- daemon store (read-only) ---------------------------------

#[derive(Default)]
struct StoreEvidence {
    /// `(sha, verdict, reviewer)` rows from the `verdicts` table.
    verdicts: Vec<(String, String, String)>,
    /// `(kind, at)` events.
    events: Vec<(String, f64)>,
    opened: bool,
    /// `approval_recorded` events on the approval stream, oldest first.
    approvals: Vec<ApprovalRec>,
    /// The first `approval_revoked` event per approval id.
    revocations: HashMap<String, Revocation>,
    /// Why the approval stream could not be read — approval state is
    /// then `unknown`, never `missing`. `None` when it answered.
    approvals_gap: Option<String>,
}

/// One operator approval record (CAD-217) — `store::NewApproval` as
/// persisted, plus the event time.
#[derive(Debug, Clone, Default)]
struct ApprovalRec {
    id: String,
    source: String,
    action: String,
    head_sha: String,
    repo: String,
    pr: u64,
    recorded_via: Option<String>,
    at: f64,
}

/// One operator revocation of an approval id.
#[derive(Debug, Clone, Default)]
struct Revocation {
    source: String,
    reason: String,
    at: f64,
}

/// A row's approval binding: `approved` (a record for the exact landed
/// head, recorded before the merge and not revoked before it),
/// `revoked` (every such record was revoked before the merge),
/// `missing` (the store answered and no record was in force at merge
/// time), `unknown` (the evidence could not be read), or `not-required`
/// (a non-human class with no record).
#[derive(Debug, Default)]
struct ApprovalView {
    state: String,
    reason: Option<String>,
    /// The record the state rests on (for `missing`: a post-merge one).
    record: Option<ApprovalRec>,
    revocation: Option<Revocation>,
    /// Records for this PR that name a different head — shown, never
    /// counted: an approval does not carry over to a later head.
    other_heads: Vec<ApprovalRec>,
}

fn store_evidence(path: &Path) -> StoreEvidence {
    let mut ev = StoreEvidence::default();
    if !path.exists() {
        // A host that never ran the daemon has no store — the table
        // is empty, not unreadable. No evidence gap for verdicts; but
        // approval records live nowhere else, so this host cannot say
        // whether one exists.
        ev.approvals_gap = Some(format!(
            "no daemon store at {} — approval records live there",
            path.display()
        ));
        return ev;
    }
    let Ok(conn) = crate::store::open_read_only(path) else {
        ev.approvals_gap = Some(format!("store {} unreadable", path.display()));
        return ev;
    };
    ev.opened = true;
    // Bounded reads — an audit must not pull a whole event history
    // into memory; only recent verdicts and restart-kind events are
    // evidence anyway.
    if let Ok(mut st) =
        conn.prepare("SELECT sha, verdict, reviewer FROM verdicts ORDER BY seq DESC LIMIT 5000")
    {
        ev.verdicts = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
    }
    if let Ok(mut st) = conn.prepare(
        "SELECT kind, at FROM events WHERE kind LIKE '%restart%' ORDER BY seq DESC LIMIT 5000",
    ) {
        ev.events = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
    }
    read_approvals(&conn, &mut ev);
    ev
}

/// The approval stream, read-only. Malformed payloads are skipped —
/// only the store's validated writer puts rows on this stream, so a
/// row that does not parse proves nothing either way.
fn read_approvals(conn: &rusqlite::Connection, ev: &mut StoreEvidence) {
    use crate::store::{APPROVAL_RECORDED_EVENT, APPROVAL_REVOKED_EVENT, APPROVAL_STREAM};
    let rows: rusqlite::Result<Vec<(String, String, f64)>> = conn
        .prepare("SELECT kind, payload, at FROM events WHERE alias=? ORDER BY seq LIMIT 100000")
        .and_then(|mut st| {
            st.query_map([APPROVAL_STREAM], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        });
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            ev.approvals_gap = Some(format!("approval stream unreadable: {e}"));
            return;
        }
    };
    for (kind, raw, at) in rows {
        let Ok(p) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let text = |k: &str| p[k].as_str().map(str::to_string);
        let Some(id) = text("approval_id") else {
            continue;
        };
        if kind == APPROVAL_RECORDED_EVENT {
            let (Some(source), Some(action), Some(head_sha), Some(repo), Some(pr)) = (
                text("source"),
                text("action"),
                text("head_sha"),
                p["scope"]["repo"].as_str().map(str::to_string),
                p["scope"]["pr"].as_u64(),
            ) else {
                continue;
            };
            ev.approvals.push(ApprovalRec {
                id,
                source,
                action,
                head_sha,
                repo,
                pr,
                recorded_via: text("recorded_via"),
                at,
            });
        } else if kind == APPROVAL_REVOKED_EVENT {
            let (Some(source), Some(reason)) = (text("source"), text("reason")) else {
                continue;
            };
            ev.revocations
                .entry(id)
                .or_insert(Revocation { source, reason, at });
        }
    }
}

fn enrich_store(ev: &StoreEvidence, path: &Path, row: &mut Row) {
    if path.exists() && !ev.opened {
        let reason = format!("store {} unreadable", path.display());
        row.evidence_gaps.push(reason.clone());
        row.unknowns.push(("verdict".into(), reason));
    }
    for (sha, verdict, reviewer) in &ev.verdicts {
        let hit = row
            .landed_head
            .as_deref()
            .or(row.reviewed_head.as_deref())
            .is_some_and(|h| h.len() >= 7 && (h.starts_with(sha) || sha.starts_with(h)));
        if hit {
            row.verdict = row.verdict.clone().or(Some(verdict.clone()));
            row.reviewer = row.reviewer.clone().or(Some(reviewer.clone()));
        }
    }
}

// ---------- operator approval (CAD-217) -----------------------------

/// Bind the row to the operator approval in force at merge time for
/// the exact landed head, then decide what the row's class makes of it.
/// Only human-class rows can flag; other rows show a bound record for
/// context and are otherwise `not-required` (or `unknown` without a
/// risk class).
fn enrich_approval(ev: &StoreEvidence, slug: Option<&str>, row: &mut Row) {
    let view = bind_approval(ev, slug, row);
    row.approval = match row.class.as_deref() {
        Some("human") => {
            if view.state == "unknown" {
                row.unknowns
                    .push(("approval".into(), view.reason.clone().unwrap_or_default()));
            }
            view
        }
        _ if view.record.is_some() && view.state != "missing" => view,
        Some(_) => ApprovalView {
            state: "not-required".into(),
            other_heads: view.other_heads,
            ..Default::default()
        },
        None => ApprovalView {
            state: "unknown".into(),
            reason: Some(
                view.reason
                    .filter(|_| view.state == "unknown")
                    .unwrap_or_else(|| {
                        "no risk class recorded — whether the human gate applied is unknown".into()
                    }),
            ),
            other_heads: view.other_heads,
            ..Default::default()
        },
    };
}

fn bind_approval(ev: &StoreEvidence, slug: Option<&str>, row: &Row) -> ApprovalView {
    let unknown = |reason: String| ApprovalView {
        state: "unknown".into(),
        reason: Some(reason),
        ..Default::default()
    };
    if let Some(gap) = &ev.approvals_gap {
        return unknown(gap.clone());
    }
    let (Some(landed), Some(merged_at)) = (row.landed_head.as_deref(), row.merged_at) else {
        return unknown("no landed head to bind an approval to".into());
    };
    // Scope: a merge approval for this PR in this repo (the repo is
    // checked whenever the audit knows its own — fixture runs do not).
    let in_scope = |a: &&ApprovalRec| {
        a.action == "merge"
            && row.pr == Some(a.pr)
            && slug.is_none_or(|s| s.eq_ignore_ascii_case(&a.repo))
    };
    let (bound, other_heads): (Vec<&ApprovalRec>, Vec<&ApprovalRec>) = ev
        .approvals
        .iter()
        .filter(in_scope)
        .partition(|a| a.head_sha.eq_ignore_ascii_case(landed));
    let other_heads: Vec<ApprovalRec> = other_heads.into_iter().cloned().collect();
    let revoked = |a: &ApprovalRec| ev.revocations.get(&a.id).cloned();
    let before = |a: &&&ApprovalRec| a.at <= merged_at;
    let view = |state: &str, a: &ApprovalRec, reason: Option<String>| ApprovalView {
        state: state.into(),
        reason,
        record: Some(a.clone()),
        revocation: revoked(a),
        other_heads: other_heads.clone(),
    };
    if let Some(a) = bound
        .iter()
        .filter(before)
        .find(|a| revoked(a).is_none_or(|r| r.at > merged_at))
    {
        // In force at merge time. A later revocation is shown, but the
        // question is what held when the merge happened.
        let reason = revoked(a).map(|r| format!("revoked after the merge ({})", iso(r.at)));
        return view("approved", a, reason);
    }
    if let Some(a) = bound.iter().find(before) {
        let reason = format!("approval {} was revoked before the merge", a.id);
        return view("revoked", a, Some(reason));
    }
    if let Some(a) = bound.first() {
        let reason = format!(
            "only a post-merge record names the landed head (approval {} recorded {})",
            a.id,
            iso(a.at)
        );
        return view("missing", a, Some(reason));
    }
    let reason = match other_heads.first() {
        Some(o) => format!(
            "no approval names landed head {} — approval {} names head {}, and an \
             approval never carries over to another head",
            short(landed),
            o.id,
            short(&o.head_sha)
        ),
        None => format!("no approval record names landed head {}", short(landed)),
    };
    ApprovalView {
        state: "missing".into(),
        reason: Some(reason),
        other_heads,
        ..Default::default()
    }
}

fn short(sha: &str) -> &str {
    &sha[..9.min(sha.len())]
}

fn iso(at: f64) -> String {
    crate::issue::time::iso(at as i64)
}

// ---------- tracker --------------------------------------------------

fn enrich_tracker(pm: Option<&Path>, row: &mut Row) {
    let Some(pm) = pm else { return };
    for id in &row.issues {
        if let Ok(entries) = std::fs::read_dir(pm) {
            for ent in entries.flatten() {
                if ent.path().join(id).join("issue.md").exists() {
                    if let Some(key) = ent.file_name().to_str() {
                        row.project = Some(key.to_string());
                    }
                }
            }
        }
    }
}

// ---------- merge-content + post-merge evidence -----------------------

/// Does the merge commit carry the reviewed head's change? Real merge
/// commits: ancestry. Squashes: the merge's patch-id vs the reviewed
/// diff's patch-id.
fn contains_head(repo: &Path, row: &mut Row) {
    // Verify the *reviewed* head when a verdict named one — `headRefOid`
    // is mutable (a push to an undeleted branch moves it), so the note's
    // binding is the stable claim to check. With no note, the landed
    // head is all there is.
    let Some(head) = row
        .reviewed_head
        .clone()
        .or_else(|| row.landed_head.clone())
    else {
        row.contains_head = "unknown (no head recorded)".into();
        return;
    };
    if git(repo, &["cat-file".into(), "-e".into(), head.clone()]).is_err() {
        row.contains_head = "unknown (head not in local object store — never fetched)".into();
        row.unknowns
            .push(("contains_head".into(), format!("{head} not in local clone")));
        return;
    }
    // Direct ancestry — true merges and fast-forwards.
    if git(
        repo,
        &[
            "merge-base".into(),
            "--is-ancestor".into(),
            head.clone(),
            row.merge_sha.clone(),
        ],
    )
    .is_ok()
    {
        row.contains_head = "yes (ancestor)".into();
        row.tree_match = "yes (ancestor)".into();
        return;
    }
    // Squash: patch-id of the squash delta vs the reviewed diff.
    let merge_patch = patch_id(repo, &format!("{}^..{}", row.merge_sha, row.merge_sha));
    let base = git(
        repo,
        &[
            "merge-base".into(),
            format!("{}^", row.merge_sha),
            head.clone(),
        ],
    )
    .ok()
    .map(|s| s.trim().to_string());
    let head_patch = base.and_then(|b| patch_id(repo, &format!("{b}..{head}")));
    match (merge_patch, head_patch) {
        (Some(m), Some(h)) if m == h => {
            row.contains_head = "yes (patch-id match)".into();
            row.tree_match = "yes (patch-id match)".into();
        }
        (Some(_), Some(_)) => {
            row.contains_head = "no (patch-id differs)".into();
            row.tree_match = "no (patch-id differs)".into();
        }
        _ => {
            row.contains_head = "unknown (patch-id unavailable)".into();
            row.unknowns
                .push(("contains_head".into(), "patch-id comparison failed".into()));
        }
    }
}

/// Stable patch-id for a diff range (`git diff <range> | git patch-id`).
/// The `diff` half is `run_bounded`; `patch-id` reads a bounded in-memory
/// buffer, so its unbounded-looking wait is CPU-only.
fn patch_id(repo: &Path, range: &str) -> Option<String> {
    use std::io::Write;
    let mut diff = Command::new("git");
    diff.arg("-C").arg(repo).args(["diff", "--patch", range]);
    let out = crate::proc::run_bounded(&mut diff, GIT_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let mut pid = Command::new("git");
    let mut child = pid
        .arg("patch-id")
        .arg("--stable")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&out.stdout).ok()?;
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().next().map(str::to_string)
}

/// Every `(sha, body)` on the default branch — scanned once for the
/// per-row revert check in `post_merge`. Bounded: a revert of a merge
/// older than the newest 10k commits is archaeological, not evidence.
fn branch_log(repo: &Path, default_ref: &str) -> Vec<(String, String)> {
    git(
        repo,
        &[
            "log".into(),
            "-n".into(),
            "10000".into(),
            "--format=%H%x1f%B%x1e".into(),
            default_ref.into(),
        ],
    )
    .map(|log| {
        log.split('\x1e')
            .filter_map(|entry| {
                let mut p = entry.split('\x1f');
                Some((p.next()?.to_string(), p.next().unwrap_or("").to_string()))
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Post-merge outcome: later `Revert` commits on the default branch,
/// then daemon events for restart markers near the merge.
fn post_merge(log: &[(String, String)], ev: &StoreEvidence, row: &mut Row) {
    // Revert: a later commit whose body says `This reverts commit <sha>`
    // or whose subject reverts the PR title.
    for (sha, body) in log {
        if sha == &row.merge_sha {
            continue;
        }
        let reverted = body.contains(&format!("This reverts commit {}", row.merge_sha))
            || row
                .pr
                .is_some_and(|n| body.contains("Revert") && body.contains(&format!("#{n}")));
        if reverted {
            row.revert = format!("yes ({})", &sha[..7.min(sha.len())]);
            break;
        }
    }
    // Daemon restart: an event named restart near the merge — the
    // event feed is the daemon's own record; absent kinds stay unknown.
    if let Some(at) = row.merged_at {
        for (kind, t) in &ev.events {
            if kind.contains("restart") && *t >= at && *t <= at + POST_MERGE_WINDOW_SECS {
                row.daemon_restart = format!("yes (event {kind})");
            }
        }
    }
    for (field, val) in [
        ("smoke", &row.smoke),
        ("daemon_restart", &row.daemon_restart),
    ] {
        if val.starts_with("unknown") {
            row.unknowns.push((
                field.into(),
                "no ops-merge note or daemon event records it".into(),
            ));
        }
    }
}

// ---------- flags -----------------------------------------------------

fn flag_row(row: &mut Row) {
    // `reviewer==merger` compares GitHub identities only: the
    // `qa-verdict` status's `creator.login` against `mergedBy.login`.
    // A verdict note's `From:` is an agent alias — a different
    // namespace that is rendered but never feeds the flag (an alias
    // string-matching a login would be coincidence, not proof).
    let same_identity = row
        .qa_verdict_creator
        .as_deref()
        .zip(row.merger.as_deref())
        .is_some_and(|(a, b)| !a.is_empty() && a.eq_ignore_ascii_case(b));
    if same_identity {
        row.flags.push("reviewer==merger".into());
    }
    // A passing verdict on the exact head that landed: the verdict
    // note/table says pass AND the head it names is the squash-merged
    // `headRefOid` — or the `qa-verdict` commit status is SUCCESS on
    // that head. A verdict written after the merge (or a status set
    // post-merge) does not clear it: at merge time the head was
    // unreviewed.
    let verdict_pass = row
        .verdict
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case("pass"))
        && !row.verdict_post_hoc;
    // Heads "agree" when the strings match — or when the merge-content
    // check proved the reviewed change landed anyway (`headRefOid`
    // moves if an undeleted branch is pushed; the patch-id is the
    // stable evidence).
    let heads_agree = match (&row.reviewed_head, &row.landed_head) {
        (Some(r), Some(l)) => {
            r.len() >= 7
                && (l.starts_with(&r[..r.len().min(l.len())])
                    || r.starts_with(&l[..l.len().min(r.len())]))
        }
        // With no reviewed head recorded there is nothing to bind —
        // the merge_sha is what a note would name.
        (None, _) => true,
        _ => false,
    } || row.contains_head.starts_with("yes");
    let status_ok = row
        .qa_verdict_status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("SUCCESS"))
        && !row.status_post_hoc;
    // The flag is "sources answered and said no". When a channel the
    // verdict could come through did not answer — gh down, notes dir
    // unreadable, store unopenable, the reviewed head absent from the
    // clone — the row is unknown, never an accusation.
    let mut gaps = !row.evidence_gaps.is_empty();
    if !heads_agree && row.reviewed_head.is_some() && row.contains_head.starts_with("unknown") {
        // The note named a head we cannot verify against the merge —
        // the binding question is unanswerable, not negative.
        gaps = true;
    }
    if !(verdict_pass && heads_agree) && !status_ok && !row.is_root && !gaps {
        row.flags.push("no-passing-verdict".into());
    }
    // The human-class gate (CAD-217): an operator approval for the
    // exact landed head must have been in force at merge time. An
    // unreadable store is `unknown` — reported, never flagged.
    if row.class.as_deref() == Some("human") {
        match row.approval.state.as_str() {
            "missing" => row.flags.push("approval-missing".into()),
            "revoked" => row.flags.push("approval-revoked".into()),
            _ => {}
        }
    }
}

// ---------- rendering -------------------------------------------------

fn approval_rec_json(a: &ApprovalRec) -> Value {
    json!({
        "id": a.id, "source": a.source, "action": a.action,
        "head_sha": a.head_sha,
        "scope": {"repo": a.repo, "pr": a.pr},
        "recorded_via": a.recorded_via, "recorded_at": a.at,
    })
}

fn approval_json(row: &Row) -> Value {
    let v = &row.approval;
    let mut j = json!({
        "required": match row.class.as_deref() {
            Some(c) => json!(c == "human"),
            None => Value::Null,
        },
        "state": v.state,
        "reason": v.reason,
        "record": v.record.as_ref().map(approval_rec_json),
        "before_merge": v.record.as_ref().zip(row.merged_at).map(|(a, m)| a.at <= m),
        "revocation": v.revocation.as_ref().map(|r| json!({
            "source": r.source, "reason": r.reason, "revoked_at": r.at,
            "before_merge": row.merged_at.map(|m| r.at <= m),
        })),
        "other_heads": v.other_heads.iter().map(approval_rec_json).collect::<Vec<_>>(),
    });
    if v.state.is_empty() {
        j["state"] = json!("unknown");
    }
    j
}

/// The text row's approval line — for human-class rows, and for any
/// row an approval record binds to.
fn approval_line(row: &Row) -> Option<String> {
    let v = &row.approval;
    if row.class.as_deref() != Some("human") && v.record.is_none() {
        return None;
    }
    let mut line = format!("approval {}", v.state);
    if let Some(a) = &v.record {
        let when = match row.merged_at {
            Some(m) if a.at <= m => "before merge",
            Some(_) => "after merge",
            None => "merge time unknown",
        };
        line.push_str(&format!(
            " · id {} · source \"{}\" · head {} · recorded {} ({when})",
            a.id,
            a.source,
            short(&a.head_sha),
            iso(a.at)
        ));
    }
    if let Some(r) = &v.revocation {
        line.push_str(&format!(
            " · revoked {} by \"{}\": {}",
            iso(r.at),
            r.source,
            r.reason
        ));
    }
    if let Some(reason) = &v.reason {
        line.push_str(&format!(" — {reason}"));
    }
    Some(line)
}

fn row_json(row: &Row) -> Value {
    let unknowns: Vec<Value> = row
        .unknowns
        .iter()
        .map(|(f, r)| json!({"field": f, "reason": r}))
        .collect();
    json!({
        "pr": row.pr,
        "title": row.title,
        "merge_sha": row.merge_sha,
        "merged_at": row.merged_at,
        "landed_head": row.landed_head,
        "reviewed_head": row.reviewed_head,
        "contains_head": row.contains_head,
        "qa_verdict_status": row.qa_verdict_status,
        "qa_verdict_creator": row.qa_verdict_creator,
        "status_post_hoc": row.status_post_hoc,
        "verdict": row.verdict,
        "verdict_post_hoc": row.verdict_post_hoc,
        "reviewer": row.reviewer,
        "merger": row.merger,
        "class": row.class,
        "trigger": row.trigger,
        "gates": row.gates,
        "gate_summary": {
            "suite": row.gate_suite,
            "stress": row.gate_stress,
            "flakes": row.gate_flakes,
        },
        "auditor_check": row.auditor_check,
        "residue": row.residue,
        "project": row.project,
        "issues": row.issues,
        "outcome": {
            "tree_match": row.tree_match,
            "smoke": row.smoke,
            "daemon_restart": row.daemon_restart,
            "revert": row.revert,
        },
        "flags": row.flags,
        "approval": approval_json(row),
        "evidence_unavailable": if row.evidence_gaps.is_empty() {
            Value::Null
        } else {
            json!(row.evidence_gaps.join("; "))
        },
        "unknowns": unknowns,
    })
}

fn print_json(
    repo: &Path,
    default_ref: &str,
    rows: &[Row],
    flagged: usize,
    opts: &AuditOptions,
    since: f64,
) {
    let merges: Vec<Value> = rows.iter().map(row_json).collect();
    let mut by_class = json!({});
    for r in rows {
        let k = r.class.clone().unwrap_or_else(|| "unknown".into());
        by_class[k] = json!(by_class[&k].as_u64().unwrap_or(0) + 1);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema": "cadence.audit/1",
            "repo": repo.display().to_string(),
            "default_ref": default_ref,
            "since": opts.since,
            "since_epoch": if since > 0.0 { json!(since) } else { Value::Null },
            "filters": {"class": opts.class, "project": opts.project, "limit": opts.limit},
            "merges": merges,
            "summary": {
                "rows": rows.len(),
                "flagged": flagged,
                "flags": rows.iter().flat_map(|r| r.flags.clone()).collect::<Vec<_>>(),
                "by_class": by_class,
            },
        }))
        .unwrap_or_default()
    );
}

fn render_text(
    repo: &Path,
    default_ref: &str,
    rows: &[Row],
    flagged: usize,
    opts: &AuditOptions,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let or = |o: &Option<String>| o.clone().unwrap_or_else(|| "unknown".into());
    let _ = writeln!(
        out,
        "AUDIT {} ({} merges on {}){}",
        repo.display(),
        rows.len(),
        default_ref,
        opts.since
            .as_deref()
            .map(|s| format!(" since {s}"))
            .unwrap_or_default()
    );
    if rows.is_empty() {
        let _ = writeln!(out, "  no merges in range");
    }
    for row in rows {
        let flag = if row.flags.is_empty() {
            String::new()
        } else {
            format!("  FLAG[{}]", row.flags.join(","))
        };
        let pr = row
            .pr
            .map(|n| format!("#{n}"))
            .unwrap_or_else(|| "?".into());
        let _ = writeln!(out, "{pr} {}{}", row.title, flag);
        if !row.evidence_gaps.is_empty() {
            let _ = writeln!(
                out,
                "    unknown — evidence unavailable: {}",
                row.evidence_gaps.join("; ")
            );
        }
        let _ = writeln!(
            out,
            "    merge {} · merged_at {} · merger {}",
            &row.merge_sha[..9.min(row.merge_sha.len())],
            row.merged_at
                .map(|t| crate::issue::time::iso(t as i64))
                .unwrap_or_else(|| "unknown".into()),
            or(&row.merger),
        );
        let _ = writeln!(
            out,
            "    reviewed_head {} · landed_head {} · contains_head {}",
            row.reviewed_head
                .as_deref()
                .map(|s| &s[..9.min(s.len())])
                .unwrap_or("unknown"),
            row.landed_head
                .as_deref()
                .map(|s| &s[..9.min(s.len())])
                .unwrap_or("unknown"),
            row.contains_head,
        );
        let _ = writeln!(
            out,
            "    verdict {}{} · qa-verdict {}{} · reviewer {} · reviewer@gh {}",
            or(&row.verdict),
            if row.verdict_post_hoc {
                " (post-merge)"
            } else {
                ""
            },
            or(&row.qa_verdict_status),
            if row.status_post_hoc {
                " (post-merge)"
            } else {
                ""
            },
            or(&row.reviewer),
            or(&row.qa_verdict_creator),
        );
        let _ = writeln!(
            out,
            "    class {} · trigger {}",
            or(&row.class),
            or(&row.trigger),
        );
        if row.gates.is_empty() {
            let _ = writeln!(out, "    gates unknown (no gate summary in verdict note)");
        } else {
            let _ = writeln!(
                out,
                "    gates suite={} stress={} flakes={}",
                row.gate_suite.as_deref().unwrap_or("none recorded"),
                row.gate_stress.as_deref().unwrap_or("none recorded"),
                row.gate_flakes.as_deref().unwrap_or("none recorded"),
            );
        }
        if let Some(a) = &row.auditor_check {
            let _ = writeln!(out, "    auditor_check {a}");
        } else {
            let _ = writeln!(out, "    auditor_check unknown (none recorded)");
        }
        if !row.residue.is_empty() {
            let _ = writeln!(out, "    residue {}", row.residue.join(" "));
        }
        let _ = writeln!(
            out,
            "    outcome tree_match={} smoke={} daemon_restart={} revert={}",
            row.tree_match, row.smoke, row.daemon_restart, row.revert,
        );
        if let Some(line) = approval_line(row) {
            let _ = writeln!(out, "    {line}");
        }
        for (field, reason) in &row.unknowns {
            let _ = writeln!(out, "    unknown[{field}] {reason}");
        }
    }
    let _ = writeln!(
        out,
        "summary: {} row(s), {} flagged{}{}",
        rows.len(),
        flagged,
        if flagged > 0 { " — " } else { "" },
        if flagged > 0 {
            rows.iter()
                .flat_map(|r| r.flags.clone())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            String::new()
        },
    );
    out
}

// ---------- time ------------------------------------------------------

/// `24h`, `7d`, `2w`, `YYYY-MM-DD`, or a unix epoch → epoch seconds.
fn parse_since(s: &str) -> Result<f64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix('h') {
        (n, 3600.0)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86400.0)
    } else if let Some(n) = s.strip_suffix('w') {
        (n, 604800.0)
    } else {
        (s, 0.0)
    };
    if mult > 0.0 {
        let n: f64 = num
            .parse()
            .map_err(|_| Error::rejected(format!("--since '{s}': bad duration")))?;
        return Ok(crate::issue::time::now_epoch() as f64 - n * mult);
    }
    if let Ok(epoch) = s.parse::<f64>() {
        return Ok(epoch);
    }
    parse_iso(s).map(|t| t as f64).ok_or_else(|| {
        Error::rejected(format!(
            "--since '{s}': expected 24h, 7d, YYYY-MM-DD or epoch"
        ))
    })
}

/// `YYYY-MM-DD` or RFC-3339 (`2026-09-20T10:30:27Z`) → epoch seconds.
fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T').unwrap_or((s, "00:00:00"));
    let mut dp = date.split('-');
    let (y, m, d): (i64, i64, i64) = (
        dp.next()?.parse().ok()?,
        dp.next()?.parse().ok()?,
        dp.next()?.parse().ok()?,
    );
    let mut tp = time.split(':');
    let (hh, mm, ss): (i64, i64, i64) = (
        tp.next().unwrap_or("0").parse().ok()?,
        tp.next().unwrap_or("0").parse().ok()?,
        tp.next().unwrap_or("0").split('.').next()?.parse().ok()?,
    );
    // Days since epoch (Howard Hinnant's civil-from-days inverse).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_number_from_subject() {
        assert_eq!(
            pr_number("session start|end: one-verb session gate (CAD-92) (#63)"),
            Some(63)
        );
        // A trailing qualifier after the squash suffix must not hide
        // the number; real merge commits parse the prefix form.
        assert_eq!(pr_number("audit log (#63) (rebased)"), Some(63));
        assert_eq!(
            pr_number("Merge pull request #64 from favcrm/branch"),
            Some(64)
        );
        assert_eq!(pr_number("plain commit"), None);
        assert_eq!(pr_number("merge: no number (#)"), None);
    }

    #[test]
    fn issue_ids_finds_all() {
        assert_eq!(
            issue_ids("codex: approval (CAD-143, CAD-126) (#72)"),
            vec!["CAD-143".to_string(), "CAD-126".to_string()]
        );
        assert!(issue_ids("nothing").is_empty());
    }

    #[test]
    fn parse_since_forms() {
        let now = crate::issue::time::now_epoch() as f64;
        assert!((parse_since("24h").unwrap() - (now - 86400.0)).abs() < 2.0);
        assert!((parse_since("7d").unwrap() - (now - 7.0 * 86400.0)).abs() < 2.0);
        assert_eq!(parse_iso("1970-01-02").unwrap(), 86400);
        assert_eq!(parse_iso("1970-01-01T01:00:00Z").unwrap(), 3600);
        assert!(parse_since("garbage").is_err());
    }

    #[test]
    fn parse_note_extracts_fields() {
        let text = "# Verdict: CAD-92 — pass\n> From: `qa-1`\n> Issue: `CAD-92`\n\n## Verdict\npass — head `7896dd2735035c0c67e246039cb495231702941c`\n\n## Risk\n**Risk: notify (deletion paths; fourth round)**\n\n**What an auditor should check after the merge:** one handoff lands.\n\nResidue **CAD-147** stands.\n";
        let n = parse_note(Path::new("/tmp/x-verdict.md"), "x-verdict.md", text);
        assert_eq!(n.kind, "verdict");
        assert_eq!(n.from.as_deref(), Some("qa-1"));
        assert!(n.shas.iter().any(|s| s.starts_with("7896dd2")));
        assert_eq!(n.class.as_deref(), Some("notify"));
        assert!(n.trigger.as_deref().unwrap().contains("deletion"));
        assert_eq!(n.auditor_check.as_deref(), Some("one handoff lands."));
        assert!(n.residue.contains(&"CAD-147".to_string()));
    }

    #[test]
    fn hex_shas_finds_tokens() {
        let v = hex_shas("head `7896dd2735035c0c67e246039cb495231702941c` and main 07ca3013");
        assert!(v.contains(&"7896dd2735035c0c67e246039cb495231702941c".to_string()));
        assert!(v.contains(&"07ca3013".to_string()));
        // All-letter abbrevs are real (`cabbebe`); all-digit tokens stay
        // out of general context (filename dates) but count on a head line.
        assert!(hex_shas("cabbebe").contains(&"cabbebe".to_string()));
        assert!(hex_shas("20260920-141226").is_empty());
        assert!(hex_shas_ctx("5428215", true).contains(&"5428215".to_string()));
    }

    fn head_line(text: &str) -> Option<String> {
        parse_note(Path::new("/tmp/n-verdict.md"), "n-verdict.md", text).head_sha
    }

    #[test]
    fn head_line_parses_forms() {
        assert_eq!(
            head_line("pass — head `7896dd2735035c0c67e246039cb495231702941c`").as_deref(),
            Some("7896dd2735035c0c67e246039cb495231702941c")
        );
        assert_eq!(
            head_line("pass — head: 5428215").as_deref(),
            Some("5428215")
        );
        // `ahead`/`overhead` are not head lines.
        assert_eq!(head_line("pass — ahead 7896dd2"), None);
        // A case-changing multibyte char before `head` must not panic —
        // `ẞ`.len() grows under to_lowercase, so lowercase offsets
        // cannot index the original text.
        assert_eq!(
            head_line("verdict ẞ head 7896dd27").as_deref(),
            Some("7896dd27")
        );
        assert_eq!(
            head_line("verdict İ head 7896dd27").as_deref(),
            Some("7896dd27")
        );
    }

    fn flagged_row() -> Row {
        Row {
            pr: Some(1),
            merge_sha: "ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab".into(),
            landed_head: Some("7896dd2735035c0c67e246039cb495231702941c".into()),
            contains_head: "yes (patch-id match)".into(),
            ..Default::default()
        }
    }

    #[test]
    fn flag_reviewer_eq_merger_uses_github_namespace() {
        // Same GitHub login posted the qa-verdict status and merged —
        // this is the only identity pair the flag compares.
        let mut r = flagged_row();
        r.merger = Some("cc-syntax".into());
        r.qa_verdict_creator = Some("cc-syntax".into());
        r.qa_verdict_status = Some("SUCCESS".into());
        flag_row(&mut r);
        assert!(r.flags.iter().any(|f| f == "reviewer==merger"));
        assert!(!r.flags.iter().any(|f| f == "no-passing-verdict"));

        // The note `From:` is an agent alias — matching the merger's
        // login is coincidence, not proof. It must never flag.
        let mut r = flagged_row();
        r.merger = Some("qa-1".into());
        r.reviewer = Some("qa-1".into());
        r.qa_verdict_creator = Some("qa-bot".into());
        r.qa_verdict_status = Some("SUCCESS".into());
        flag_row(&mut r);
        assert!(r.flags.is_empty(), "{:?}", r.flags);
    }

    #[test]
    fn flag_no_passing_verdict_only_when_sources_answered() {
        // Sources all answered, none proves a pass on the landed head.
        let mut r = flagged_row();
        flag_row(&mut r);
        assert_eq!(r.flags, vec!["no-passing-verdict".to_string()]);

        // gh down / notes unreadable → evidence unavailable, not an
        // accusation: no flag.
        let mut r = flagged_row();
        r.evidence_gaps
            .push("status fetch failed: gh: timeout".into());
        flag_row(&mut r);
        assert!(r.flags.is_empty());

        // Reviewed head exists but isn't in the clone — the binding is
        // unanswerable, not negative.
        let mut r = flagged_row();
        r.reviewed_head = Some("deadd00".into());
        r.contains_head = "unknown (head not in local object store — never fetched)".into();
        flag_row(&mut r);
        assert!(r.flags.is_empty());

        // A pass verdict on the landed head clears it.
        let mut r = flagged_row();
        r.verdict = Some("pass".into());
        r.reviewed_head = r.landed_head.clone();
        flag_row(&mut r);
        assert!(r.flags.is_empty(), "{:?}", r.flags);

        // A SUCCESS qa-verdict status clears it.
        let mut r = flagged_row();
        r.qa_verdict_status = Some("SUCCESS".into());
        flag_row(&mut r);
        assert!(r.flags.is_empty(), "{:?}", r.flags);
    }

    #[test]
    fn flag_post_hoc_verdict_does_not_clear() {
        // A pass verdict written after the merge proves nothing about
        // merge-time review.
        let mut r = flagged_row();
        r.verdict = Some("pass".into());
        r.reviewed_head = r.landed_head.clone();
        r.verdict_post_hoc = true;
        flag_row(&mut r);
        assert!(r.flags.iter().any(|f| f == "no-passing-verdict"));

        // Same for a status posted after the merge.
        let mut r = flagged_row();
        r.qa_verdict_status = Some("SUCCESS".into());
        r.status_post_hoc = true;
        flag_row(&mut r);
        assert!(r.flags.iter().any(|f| f == "no-passing-verdict"));
    }

    const HEAD: &str = "7896dd2735035c0c67e246039cb495231702941c";
    const OLD_HEAD: &str = "1111111111111111111111111111111111111111";
    const MERGED: f64 = 1_000_000.0;

    fn rec(id: &str, head: &str, at: f64) -> ApprovalRec {
        ApprovalRec {
            id: id.into(),
            source: "operator in chat".into(),
            action: "merge".into(),
            head_sha: head.into(),
            repo: "x/y".into(),
            pr: 1,
            recorded_via: Some("operator-connection".into()),
            at,
        }
    }

    fn revoke(at: f64) -> Revocation {
        Revocation {
            source: "operator".into(),
            reason: "head moved".into(),
            at,
        }
    }

    /// A human-class row's approval state and flags under `ev`.
    fn human(ev: &StoreEvidence, slug: Option<&str>) -> Row {
        let mut r = flagged_row();
        r.landed_head = Some(HEAD.into());
        r.merged_at = Some(MERGED);
        r.class = Some("human".into());
        r.qa_verdict_status = Some("SUCCESS".into());
        enrich_approval(ev, slug, &mut r);
        flag_row(&mut r);
        r
    }

    fn ev(approvals: Vec<ApprovalRec>, revocations: &[(&str, Revocation)]) -> StoreEvidence {
        StoreEvidence {
            approvals,
            revocations: revocations
                .iter()
                .map(|(id, r)| (id.to_string(), r.clone()))
                .collect(),
            opened: true,
            ..Default::default()
        }
    }

    #[test]
    fn approval_binds_exact_landed_head_in_force_at_merge() {
        // Approved before the merge for the exact landed head.
        let r = human(&ev(vec![rec("a", HEAD, MERGED - 60.0)], &[]), Some("x/y"));
        assert_eq!(r.approval.state, "approved");
        assert_eq!(r.approval.record.as_ref().unwrap().id, "a");
        assert!(r.flags.is_empty(), "{:?}", r.flags);
        assert!(approval_line(&r).unwrap().contains("(before merge)"));

        // Revoked AFTER the merge: it was in force when the merge ran.
        let e = ev(
            vec![rec("a", HEAD, MERGED - 60.0)],
            &[("a", revoke(MERGED + 60.0))],
        );
        let r = human(&e, None);
        assert_eq!(r.approval.state, "approved");
        assert!(r
            .approval
            .reason
            .as_deref()
            .unwrap()
            .contains("after the merge"));
        assert!(r.flags.is_empty());

        // Revoked BEFORE the merge: revoked, flagged.
        let e = ev(
            vec![rec("a", HEAD, MERGED - 60.0)],
            &[("a", revoke(MERGED - 30.0))],
        );
        let r = human(&e, None);
        assert_eq!(r.approval.state, "revoked");
        assert_eq!(r.flags, vec!["approval-revoked".to_string()]);

        // One revoked, another still in force: approved.
        let e = ev(
            vec![rec("a", HEAD, MERGED - 60.0), rec("b", HEAD, MERGED - 10.0)],
            &[("a", revoke(MERGED - 30.0))],
        );
        assert_eq!(human(&e, None).approval.state, "approved");

        // Only recorded after the merge: missing, the record shown.
        let r = human(&ev(vec![rec("a", HEAD, MERGED + 60.0)], &[]), None);
        assert_eq!(r.approval.state, "missing");
        assert!(r.approval.record.is_some());
        assert_eq!(r.flags, vec!["approval-missing".to_string()]);

        // An approval for an older head never counts for the landed one.
        let r = human(&ev(vec![rec("a", OLD_HEAD, MERGED - 60.0)], &[]), None);
        assert_eq!(r.approval.state, "missing");
        assert_eq!(r.approval.other_heads.len(), 1);
        assert!(r
            .approval
            .reason
            .as_deref()
            .unwrap()
            .contains("never carries"));
        assert_eq!(r.flags, vec!["approval-missing".to_string()]);

        // Another PR, another repo, or another action: out of scope.
        let mut other_pr = rec("a", HEAD, MERGED - 60.0);
        other_pr.pr = 2;
        let mut other_action = rec("b", HEAD, MERGED - 60.0);
        other_action.action = "deploy".into();
        let e = ev(vec![other_pr, other_action], &[]);
        assert_eq!(human(&e, None).approval.state, "missing");
        let e = ev(vec![rec("a", HEAD, MERGED - 60.0)], &[]);
        assert_eq!(human(&e, Some("other/repo")).approval.state, "missing");
        // The fixture/no-slug path still binds on head + PR.
        assert_eq!(human(&e, None).approval.state, "approved");
    }

    #[test]
    fn approval_unknown_evidence_is_reported_not_flagged() {
        let gap = StoreEvidence {
            approvals_gap: Some("store /s unreadable".into()),
            ..Default::default()
        };
        let r = human(&gap, None);
        assert_eq!(r.approval.state, "unknown");
        assert!(r.flags.is_empty(), "{:?}", r.flags);
        assert!(r.unknowns.iter().any(|(f, _)| f == "approval"));

        // No landed head to bind: unknown too.
        let mut r = flagged_row();
        r.landed_head = None;
        r.class = Some("human".into());
        r.qa_verdict_status = Some("SUCCESS".into());
        enrich_approval(&ev(vec![], &[]), None, &mut r);
        flag_row(&mut r);
        assert_eq!(r.approval.state, "unknown");
        assert!(!r.flags.iter().any(|f| f.starts_with("approval")));

        // Non-human classes never flag: not-required without a record,
        // the record shown when one binds; an unknown class is unknown.
        let e = ev(vec![rec("a", HEAD, MERGED - 60.0)], &[]);
        for (class, want) in [(Some("auto"), "approved"), (None, "approved")] {
            let mut r = flagged_row();
            r.landed_head = Some(HEAD.into());
            r.merged_at = Some(MERGED);
            r.class = class.map(str::to_string);
            enrich_approval(&e, None, &mut r);
            assert_eq!(r.approval.state, want);
        }
        let empty = ev(vec![], &[]);
        for (class, want) in [(Some("notify"), "not-required"), (None, "unknown")] {
            let mut r = flagged_row();
            r.merged_at = Some(MERGED);
            r.class = class.map(str::to_string);
            enrich_approval(&empty, None, &mut r);
            flag_row(&mut r);
            assert_eq!(r.approval.state, want);
            assert!(!r.flags.iter().any(|f| f.starts_with("approval")));
            assert!(approval_line(&r).is_none());
        }
    }

    #[test]
    fn flag_root_commit_exempt() {
        let mut r = Row {
            is_root: true,
            merge_sha: "ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab1ab".into(),
            contains_head: "unknown (no head recorded)".into(),
            ..Default::default()
        };
        flag_row(&mut r);
        assert!(r.flags.is_empty());
    }
}
