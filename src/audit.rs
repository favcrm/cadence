//! `cadence audit` — reconstruct every merge on the default branch
//! from data Cadence already stores, and flag the two patterns the PM
//! must never have to hunt for by hand:
//!
//! - `reviewer==merger` — the same identity reviewed and merged.
//! - `no-passing-verdict` — no `pass` verdict bound to the exact head
//!   that landed (`qa-verdict` status plus the verdict note must agree
//!   on the squash-merged `headRefOid`).
//!
//! Sources are read-only: merge commits on the default branch, `gh` PR
//! metadata and commit statuses, verdict/ops notes under the notes
//! directory, tracker folders, and the daemon's own event/verdict
//! tables (opened `SQLITE_OPEN_READ_ONLY`). Nothing is written at
//! merge time and nothing is written by this command — there is no
//! bookkeeping to keep in sync.
//!
//! Everything a source cannot prove is rendered `unknown` with the
//! reason — the audit never guesses provenance.

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
    /// Verdict recorded in a note or the verdicts table: `pass`,
    /// `fail`, `changes-requested`, …
    verdict: Option<String>,
    /// True when the bound verdict note's timestamp post-dates the
    /// merge — a post-hoc pass still clears the flag but is shown.
    verdict_post_hoc: bool,
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
    let notes = note_index(&notes_dir);

    // 4. Daemon store, opened read-only — events + the verdicts table.
    let store = store_evidence(&opts.state_dir.join(STORE_FILE));

    // 5. Tracker: issue id → project folder, for --project scoping.
    let pm = issue::default_dir().ok();

    let mut rows = Vec::new();
    for (merge_sha, at, parents, subject) in merges {
        if (at as f64) < since {
            continue;
        }
        let mut row = row_from_subject(&merge_sha, at, parents, &subject);
        enrich_gh(&repo, &gh, &mut row);
        enrich_store(&store, &mut row);
        enrich_notes(&notes, &mut row);
        enrich_tracker(pm.as_deref(), &mut row);
        contains_head(&repo, &mut row);
        post_merge(&branch_log, &store, &mut row);
        finalize_unknowns(&mut row);
        flag_row(&mut row);
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
    // since/class/project filters so an early cutoff can't hide them.
    let limit = opts.limit.unwrap_or(DEFAULT_LIMIT);
    if limit > 0 {
        rows.truncate(limit as usize);
    }

    let flagged = rows.iter().filter(|r| !r.flags.is_empty()).count();
    if opts.json {
        print_json(&repo, &default_ref, &rows, flagged, opts, since);
    } else {
        print_text(&repo, &default_ref, &rows, flagged, opts);
    }
    Ok(if flagged > 0 { 1 } else { 0 })
}

// ---------- git ------------------------------------------------------

fn git(repo: &Path, args: &[String]) -> std::result::Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    let out = cmd
        .output()
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
/// commit on the ref.
fn merge_commits(repo: &Path, reference: &str) -> Result<Vec<(String, i64, usize, String)>> {
    let out = git(
        repo,
        &[
            "log".into(),
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

/// The `(#NN)` suffix on a squash-merge subject.
fn pr_number(subject: &str) -> Option<u64> {
    let tail = subject.trim().rsplit('(').next()?;
    tail.strip_prefix('#')?.trim_end_matches(')').parse().ok()
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
    /// `--merge-report` set: every `gh` lookup resolves from the
    /// fixture — a miss is `unknown`, never a live call.
    fixture: bool,
}

fn slug(repo: &Path) -> Option<String> {
    let url = git(repo, &["remote".into(), "get-url".into(), "origin".into()]).ok()?;
    let norm = project::normalize_remote(url.trim());
    norm.strip_prefix("github.com/").map(str::to_string)
}

fn github(repo: &Path) -> Result<Gh> {
    let Some(slug) = slug(repo) else {
        return Ok(Gh {
            error: Some("no github.com origin remote".into()),
            ..Default::default()
        });
    };
    let mut gh = Gh::default();
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
    let out = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The `qa-verdict` context on one commit status payload. Status
/// contexts report `success`, check runs `SUCCESS` — normalized to
/// the uppercase StatusCheckRollup spelling.
fn qa_verdict_state(status: &Value) -> Option<String> {
    for ctx in status["statuses"].as_array()? {
        if ctx["context"].as_str() == Some("qa-verdict") {
            return ctx["state"].as_str().map(|s| s.to_uppercase());
        }
    }
    // Check runs arrive under `check_runs` on some endpoints.
    for run in status["check_runs"].as_array()? {
        if run["name"].as_str() == Some("qa-verdict") {
            return run["conclusion"]
                .as_str()
                .or_else(|| run["status"].as_str())
                .map(|s| s.to_uppercase());
        }
    }
    None
}

fn enrich_gh(repo: &Path, gh: &Gh, row: &mut Row) {
    let Some(n) = row.pr else { return };
    let Some(pr) = gh.prs.get(&n) else {
        if let Some(e) = &gh.error {
            row.unknowns
                .push(("merged_by".into(), format!("gh unavailable: {e}")));
            row.unknowns
                .push(("landed_head".into(), format!("gh unavailable: {e}")));
        } else {
            row.unknowns.push((
                "merged_by".into(),
                format!("no merged PR #{n} in gh pr list"),
            ));
        }
        return;
    };
    row.title = pr["title"].as_str().unwrap_or(&row.title).to_string();
    row.merger = pr["mergedBy"]["login"].as_str().map(str::to_string);
    row.landed_head = pr["headRefOid"].as_str().map(str::to_string);
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
    }
    // `qa-verdict` on the exact landed head.
    if let Some(head) = &row.landed_head {
        let status = match gh.statuses.get(head) {
            Some(s) => Some(s.clone()),
            // Fixture mode never falls back to a live call.
            None if gh.fixture => None,
            None => gh_status(repo, head).ok().flatten(),
        };
        match status.as_ref().and_then(qa_verdict_state) {
            Some(s) => row.qa_verdict_status = Some(s),
            None => row.unknowns.push((
                "qa_verdict_status".into(),
                format!("no qa-verdict status on {head}"),
            )),
        }
    }
}

fn gh_status(repo: &Path, sha: &str) -> std::result::Result<Option<Value>, String> {
    let Some(slug) = slug(repo) else {
        return Ok(None);
    };
    let text = gh_text(&["api".into(), format!("repos/{slug}/commits/{sha}/status")])?;
    serde_json::from_str::<Value>(&text)
        .map(Some)
        .map_err(|e| format!("gh api status: unreadable ({e})"))
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

fn note_index(dir: &Path) -> Vec<Note> {
    let mut notes = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return notes;
    };
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
    notes
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
            if let Some(i) = lower.find("head") {
                // `head` as a word — `ahead`/`overhead` don't count;
                // the sha follows a separator (`head 5428215`,
                // `head: `abc``, `head=…`).
                let bounded = (i == 0 || !lower.as_bytes()[i - 1].is_ascii_alphabetic())
                    && t[i + 4..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_ascii_alphanumeric());
                if bounded {
                    note.head_sha = hex_shas_ctx(&t[i + 4..], true).into_iter().next();
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
/// SHAs. In a `head …` context (`head_sha`) all-digit tokens stay: a
/// real abbrev can be all digits (`5428215`).
fn hex_shas(text: &str) -> Vec<String> {
    hex_shas_ctx(text, false)
}

fn hex_shas_ctx(text: &str, allow_digits: bool) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !(c.is_ascii_hexdigit())) {
        if (7..=40).contains(&word.len())
            && word.chars().any(|c| c.is_ascii_digit())
            && (allow_digits || word.chars().any(|c| ('a'..='f').contains(&c)))
        {
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

fn enrich_notes(notes: &[Note], row: &mut Row) {
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
        "no verdict note names this head; the qa-verdict status does not record its poster",
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
}

fn store_evidence(path: &Path) -> StoreEvidence {
    let mut ev = StoreEvidence::default();
    if !path.exists() {
        return ev;
    }
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return ev;
    };
    ev.opened = true;
    if let Ok(mut st) = conn.prepare("SELECT sha, verdict, reviewer FROM verdicts") {
        ev.verdicts = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
    }
    if let Ok(mut st) = conn.prepare("SELECT kind, at FROM events") {
        ev.events = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
    }
    ev
}

fn enrich_store(ev: &StoreEvidence, row: &mut Row) {
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
    let Some(head) = row
        .landed_head
        .clone()
        .or_else(|| row.reviewed_head.clone())
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
fn patch_id(repo: &Path, range: &str) -> Option<String> {
    let mut diff = Command::new("git");
    let mut child = diff
        .arg("-C")
        .arg(repo)
        .args(["diff", "--patch", range])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut pid = Command::new("git");
    let out = pid
        .arg("patch-id")
        .arg("--stable")
        .stdin(child.stdout.take()?)
        .output()
        .ok()?;
    let _ = child.wait();
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().next().map(str::to_string)
}

/// Every `(sha, body)` on the default branch — scanned once for the
/// per-row revert check in `post_merge`.
fn branch_log(repo: &Path, default_ref: &str) -> Vec<(String, String)> {
    git(
        repo,
        &[
            "log".into(),
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
    let eq = |a: &Option<String>, b: &Option<String>| {
        a.as_deref()
            .zip(b.as_deref())
            .is_some_and(|(x, y)| !x.is_empty() && x.eq_ignore_ascii_case(y))
    };
    if eq(&row.reviewer, &row.merger) {
        row.flags.push("reviewer==merger".into());
    }
    // A passing verdict on the exact head that landed: the verdict
    // note/table says pass AND the head it names is the squash-merged
    // `headRefOid` — or the `qa-verdict` commit status is SUCCESS on
    // that head. Neither provable → flag.
    let verdict_pass = row
        .verdict
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case("pass"));
    let heads_agree = match (&row.reviewed_head, &row.landed_head) {
        (Some(r), Some(l)) => {
            r.len() >= 7
                && (l.starts_with(&r[..r.len().min(l.len())])
                    || r.starts_with(&l[..l.len().min(r.len())]))
        }
        _ => false,
    };
    let status_ok = row
        .qa_verdict_status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("SUCCESS"));
    if !(verdict_pass && heads_agree) && !status_ok && !row.is_root {
        row.flags.push("no-passing-verdict".into());
    }
}

// ---------- rendering -------------------------------------------------

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

fn print_text(repo: &Path, default_ref: &str, rows: &[Row], flagged: usize, opts: &AuditOptions) {
    let or = |o: &Option<String>| o.clone().unwrap_or_else(|| "unknown".into());
    println!(
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
        println!("  no merges in range");
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
        println!("{pr} {}{}", row.title, flag);
        println!(
            "    merge {} · merged_at {} · merger {}",
            &row.merge_sha[..9.min(row.merge_sha.len())],
            row.merged_at
                .map(|t| crate::issue::time::iso(t as i64))
                .unwrap_or_else(|| "unknown".into()),
            or(&row.merger),
        );
        println!(
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
        println!(
            "    verdict {}{} · qa-verdict {} · reviewer {}",
            or(&row.verdict),
            if row.verdict_post_hoc {
                " (post-merge)"
            } else {
                ""
            },
            or(&row.qa_verdict_status),
            or(&row.reviewer),
        );
        println!(
            "    class {} · trigger {}",
            or(&row.class),
            or(&row.trigger),
        );
        if row.gates.is_empty() {
            println!("    gates unknown (no gate summary in verdict note)");
        } else {
            println!(
                "    gates suite={} stress={} flakes={}",
                row.gate_suite.as_deref().unwrap_or("none recorded"),
                row.gate_stress.as_deref().unwrap_or("none recorded"),
                row.gate_flakes.as_deref().unwrap_or("none recorded"),
            );
        }
        if let Some(a) = &row.auditor_check {
            println!("    auditor_check {a}");
        } else {
            println!("    auditor_check unknown (none recorded)");
        }
        if !row.residue.is_empty() {
            println!("    residue {}", row.residue.join(" "));
        }
        println!(
            "    outcome tree_match={} smoke={} daemon_restart={} revert={}",
            row.tree_match, row.smoke, row.daemon_restart, row.revert,
        );
        for (field, reason) in &row.unknowns {
            println!("    unknown[{field}] {reason}");
        }
    }
    println!(
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
    }
}
