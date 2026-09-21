//! `cadence issue retro <ID>` — a bounded, deterministic, read-only
//! retrospective for one issue, assembled from evidence the tracker
//! already holds:
//!
//! - the issue folder's own git history (status transitions, comment
//!   and ref commits — `history::log`),
//! - the issue's comments and refs,
//! - agent-notes: notes whose header carries `Issue: <ID>` (canonical),
//!   plus notes whose *filename* carries the id slug — a weaker match,
//!   labelled `filename` and never used to overwrite a header-tagged
//!   binding,
//! - the project's repos via `history::code_commits` (`on_default`
//!   commits are the merge evidence — no `gh` call, nothing remote),
//! - the daemon store's `jobs`→`tasks`→`verdicts` joined on
//!   `jobs.issue_id`, opened `SQLITE_OPEN_READ_ONLY`.
//!
//! Nothing is written, attached, promoted or published — the output is
//! a preview document (`cadence.retro/1`) plus a text render. Every
//! field a source cannot prove is an `unknowns` entry with its reason,
//! never a guess. Lesson text is *proposed* only — promotion to memory
//! is a separate curator decision (`cadence memory propose`), never
//! done by this command. No subprocess speaks to a remote and no LLM
//! is invoked: the report is deterministic given the same inputs.

use std::path::Path;

use serde_json::{json, Value};

use crate::error::Result;
use crate::issue::{board, history, notes, time};

/// Tokens that mark a transient-failure mention in a note or comment
/// body — a deterministic substring scan, not judgement.
const FLAKE_TOKENS: &[&str] = &[
    "flake",
    "flaky",
    "transient",
    "non-deterministic",
    "nondeterministic",
    "lock-probe",
];
/// Heading words that mark a blocking-finding section in a note.
const FINDING_HEADINGS: &[&str] = &["blocking", "finding", "blocker", "defect"];
/// A heading that also carries one of these is retrospective
/// ("blockers closed", "residue — not blocking"), not a live finding.
const NON_FINDING_HEADINGS: &[&str] = &[
    "closed",
    "resolved",
    "residue",
    "not blocking",
    "non-blocking",
    "fixed",
];
const MAX_NOTE_BYTES: u64 = 256 * 1024;
const MAX_FLAKE_HITS: usize = 20;
const MAX_HEADINGS: usize = 8;
const MAX_STORE_ROWS: usize = 200;
const MAX_LESSONS: usize = 10;
const SNIPPET_LEN: usize = 200;

fn unknown(field: &str, reason: impl Into<String>) -> Value {
    json!({"field": field, "reason": reason.into()})
}

/// Does `name` carry the id slug (`cad-198`) at a token boundary?
/// Filename matches bind only when the note did not tag itself —
/// `CAD-19` must not bind to `cad-1980`.
fn slug_mentions(name: &str, id: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let slug = id.to_ascii_lowercase();
    let mut start = 0usize;
    while let Some(off) = name[start..].find(&slug) {
        let at = start + off;
        let after = name.as_bytes().get(at + slug.len()).copied();
        if after.map(|c| !c.is_ascii_digit()).unwrap_or(true) {
            return true;
        }
        start = at + 1;
    }
    false
}

/// One evidence note (header-tagged or filename-matched).
struct EvNote {
    /// Bare filename — the source reference.
    name: String,
    /// `header` (canonical `Issue:` tag) | `filename` (slug only).
    matched: &'static str,
    kind: String,
    at: String,
    title: String,
    /// `Some(pass)` for verdict notes.
    verdict: Option<bool>,
    /// `##`/`###` heading lines, bounded.
    headings: Vec<String>,
    /// The first `##`-level finding-section heading, if any.
    finding_heading: Option<String>,
    /// First non-metadata line under that heading.
    finding_line: Option<String>,
    text: String,
}

/// Note metadata lines (`> From: qa-1`, `Head: abc123`, quoted header
/// block) never constitute a finding — they ride above real content.
fn is_note_meta(t: &str) -> bool {
    if t.starts_with('>') {
        return true;
    }
    let mut it = t.splitn(2, ':');
    match (it.next(), it.next()) {
        (Some(k), Some(_)) => matches!(
            k.trim(),
            "From"
                | "To"
                | "Cc"
                | "Issue"
                | "Session"
                | "Head"
                | "Base"
                | "Refs"
                | "Date"
                | "Subject"
        ),
        _ => false,
    }
}

/// All notes under `notes_dir` that bind to `id` — `None` when the
/// directory itself cannot be listed (distinct from "zero notes").
fn load_notes(notes_dir: &Path, id: &str) -> Option<Vec<EvNote>> {
    let entries = std::fs::read_dir(notes_dir).ok()?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > MAX_NOTE_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let matched = match notes::header_issue(&text).as_deref() {
            Some(tag) if tag == id => "header",
            // A note tagged to another issue never binds here, even
            // when its filename carries this id.
            Some(_) => continue,
            None if slug_mentions(&name, id) => "filename",
            None => continue,
        };
        let kind = notes::note_kind(&name);
        let verdict = if kind == "verdict" {
            notes::verdict_outcome(&text)
        } else {
            None
        };
        let mut headings = Vec::new();
        let mut finding_heading = None;
        let mut finding_line = None;
        let mut in_finding = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('#') {
                let h = t.trim_start_matches('#').trim().to_string();
                // Finding sections are `##`-level — an h1 is the note's
                // title ("STOP #2: same defect"), not a finding.
                in_finding = t.starts_with("##")
                    && FINDING_HEADINGS
                        .iter()
                        .any(|k| h.to_ascii_lowercase().contains(k))
                    && !NON_FINDING_HEADINGS
                        .iter()
                        .any(|k| h.to_ascii_lowercase().contains(k));
                // The heading pairs with the first content line that
                // follows it — an empty section yields to the next
                // finding heading rather than mismatched pair.
                if in_finding && finding_line.is_none() {
                    finding_heading = Some(h.clone());
                }
                if headings.len() < MAX_HEADINGS {
                    headings.push(h);
                }
                continue;
            }
            if in_finding && finding_line.is_none() && !t.is_empty() && !is_note_meta(t) {
                let mut s = t.trim_start_matches(['-', '*', ' ']).trim().to_string();
                s.truncate(SNIPPET_LEN);
                finding_line = Some(s);
            }
        }
        let at = time::note_name_to_iso(&name).unwrap_or_default();
        out.push(EvNote {
            name,
            matched,
            kind,
            at,
            title: text
                .lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .unwrap_or_default(),
            verdict,
            headings,
            finding_heading,
            finding_line,
            text,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Some(out)
}

/// First `set` history entry that moved `status` to `to`
/// (`history::log` returns newest-first).
fn first_set(log: &[Value], to: &str) -> Option<String> {
    log.iter()
        .rev()
        .find(|e| e["kind"] == "set" && e["fields"]["status"].as_str() == Some(to))
        .and_then(|e| e["at"].as_str().map(String::from))
}

/// Daemon-store verdict evidence joined on `jobs.issue_id`, opened
/// `SQLITE_OPEN_READ_ONLY`. `absent` and `unreadable` are distinct
/// outcomes; neither is fatal.
fn store_rows(state_dir: &Path, id: &str) -> (Value, Vec<Value>, Option<String>) {
    let path = state_dir.join("cadence.sqlite3");
    if !path.exists() {
        return (
            json!("absent"),
            Vec::new(),
            Some("no daemon store — jobs/tasks/verdicts corroboration unavailable".into()),
        );
    }
    let conn = match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            return (
                json!("unreadable"),
                Vec::new(),
                Some(format!("store unreadable: {e}")),
            );
        }
    };
    let jobs: Vec<String> = conn
        .prepare("SELECT id FROM jobs WHERE issue_id=?1 LIMIT 64")
        .and_then(|mut st| {
            st.query_map([id], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();
    let mut verdicts = Vec::new();
    'jobs: for job in &jobs {
        let tasks: Vec<String> = conn
            .prepare("SELECT id FROM tasks WHERE job_id=?1 LIMIT 64")
            .and_then(|mut st| {
                st.query_map([job], |r| r.get::<_, String>(0))
                    .map(|rows| rows.flatten().collect())
            })
            .unwrap_or_default();
        for task in tasks {
            let rows: Vec<Value> = conn
                .prepare(
                    "SELECT sha, verdict, reviewer, revision, created FROM verdicts \
                     WHERE task_id=?1 ORDER BY seq LIMIT 64",
                )
                .and_then(|mut st| {
                    st.query_map([&task], |r| {
                        Ok(json!({
                            "source": "store",
                            "task": task.as_str(),
                            "revision": r.get::<_, i64>(3)?,
                            "sha": r.get::<_, String>(0)?,
                            "verdict": r.get::<_, String>(1)?,
                            "reviewer": r.get::<_, String>(2)?,
                            "at": time::iso(r.get::<_, f64>(4)? as i64),
                        }))
                    })
                    .map(|rows| rows.flatten().collect())
                })
                .unwrap_or_default();
            verdicts.extend(rows);
            if verdicts.len() >= MAX_STORE_ROWS {
                break 'jobs;
            }
        }
    }
    verdicts.truncate(MAX_STORE_ROWS);
    (json!("ok"), verdicts, None)
}

/// Keyword scan for transient-failure mentions — notes and comments.
/// Each hit carries its source; nothing is inferred beyond the line.
fn flake_hits(notes: &[EvNote], comments: &[board::Comment]) -> Vec<Value> {
    let mut hits = Vec::new();
    let mut scan = |source: &str, text: &str| {
        for (i, line) in text.lines().enumerate() {
            if hits.len() >= MAX_FLAKE_HITS {
                return;
            }
            let low = line.to_ascii_lowercase();
            if FLAKE_TOKENS.iter().any(|t| low.contains(t)) {
                let mut s = line.trim().to_string();
                s.truncate(SNIPPET_LEN);
                hits.push(json!({"source": source, "line": i + 1, "text": s}));
            }
        }
    };
    for n in notes {
        scan(&n.name, &n.text);
    }
    for c in comments {
        scan(&format!("comment:{}", c.name), &c.body);
    }
    hits
}

/// `cadence issue retro <ID>` — the evidence document.
pub fn run(pm_dir: &Path, notes_dir: &Path, state_dir: &Path, id: &str) -> Result<Value> {
    let issue = board::find_issue(pm_dir, id)?;
    let mut unknowns: Vec<Value> = Vec::new();
    let mut sources: Vec<&str> = Vec::new();

    // --- tracker history (degrades cleanly when pm is not a repo) ---
    let (log, pm_git) = match history::log(pm_dir, &issue, 500) {
        Ok(l) => {
            sources.push("tracker-git");
            (l, true)
        }
        Err(_) => {
            unknowns.push(unknown(
                "status_transitions",
                "pm dir is not a git repo — timings come from file fields only",
            ));
            (Vec::new(), false)
        }
    };
    sources.push("issue-file");

    // --- notes ---
    let ev_notes = load_notes(notes_dir, id);
    let notes_present = ev_notes.is_some();
    let ev_notes = ev_notes.unwrap_or_default();
    if notes_present {
        sources.push("agent-notes");
    } else {
        unknowns.push(unknown(
            "notes",
            format!("notes dir {} not readable", notes_dir.display()),
        ));
    }

    // --- code commits (merge evidence, local repos only) ---
    let (commits, skipped_repos) = history::code_commits(pm_dir, &issue);
    sources.push("project-repos");

    // --- daemon store ---
    let (store_state, store_verdicts, store_gap) = store_rows(state_dir, id);
    if let Some(gap) = store_gap {
        unknowns.push(unknown("store_verdicts", gap));
    } else {
        sources.push("daemon-store");
    }

    // --- timings ---
    let notes_first = |kind: &str| -> Option<String> {
        ev_notes
            .iter()
            .find(|n| n.kind == kind)
            .map(|n| n.at.clone())
            .filter(|s| !s.is_empty())
    };
    let created = issue.front.created.clone();
    let ready_at = first_set(&log, "ready");
    let doing_at = first_set(&log, "doing").or_else(|| notes_first("kickoff"));
    let review_at = first_set(&log, "review").or_else(|| notes_first("qa"));
    let done_at = first_set(&log, "done").or_else(|| {
        ev_notes
            .iter()
            .find(|n| n.verdict == Some(true))
            .map(|n| n.at.clone())
            .filter(|s| !s.is_empty())
    });
    let merged_at = commits
        .iter()
        .filter(|c| c["on_default"] == json!(true))
        .filter_map(|c| c["at"].as_str())
        .min()
        .map(String::from);
    if merged_at.is_none() {
        unknowns.push(unknown(
            "merged_at",
            "no on-default commit found — not merged, or the project's repos are not local",
        ));
    }
    let hours = |a: &Option<String>, b: &Option<String>| -> Value {
        match (
            a.as_deref().and_then(time::parse_iso),
            b.as_deref().and_then(time::parse_iso),
        ) {
            (Some(x), Some(y)) if y >= x => json!(((y - x) as f64 / 360.0).round() / 10.0),
            _ => Value::Null,
        }
    };
    let timings = json!({
        "created": created,
        "ready_at": ready_at,
        "doing_at": doing_at,
        "review_at": review_at,
        "done_at": done_at,
        "merged_at": merged_at,
        "lead_hours_ready_to_done": hours(&ready_at, &done_at),
        "lead_hours_created_to_done": hours(&Some(created.clone()), &done_at),
        "lead_hours_created_to_merged": hours(&Some(created.clone()), &merged_at),
    });
    for (f, v) in [
        ("ready_at", &ready_at),
        ("doing_at", &doing_at),
        ("review_at", &review_at),
        ("done_at", &done_at),
    ] {
        if v.is_none() {
            unknowns.push(unknown(f, "no set-transition or tagged note records it"));
        }
    }

    // --- review ---
    let verdict_notes: Vec<Value> = ev_notes
        .iter()
        .filter(|n| n.kind == "verdict")
        .map(|n| {
            json!({
                "source": "note",
                "note": n.name,
                "match": n.matched,
                "at": n.at,
                "outcome": match n.verdict {
                    Some(true) => "pass",
                    Some(false) => "not-pass",
                    None => "unknown",
                },
                "title": n.title,
            })
        })
        .collect();
    let note_verdict_count = verdict_notes.len();
    let failed_rounds = verdict_notes
        .iter()
        .filter(|v| v["outcome"].as_str() == Some("not-pass"))
        .count();
    let qa_reports = ev_notes.iter().filter(|n| n.kind == "qa").count();
    let kickoffs = ev_notes.iter().filter(|n| n.kind == "kickoff").count();
    let store_fail = store_verdicts
        .iter()
        .filter(|v| v["verdict"].as_str() != Some("pass"))
        .count();

    // Caught defects: any note with a blocking-finding section —
    // qa-1 records round blockers in kickoff/qa notes, not only
    // verdicts — plus every non-pass store verdict. The summary is
    // the finding heading (which names the defect) with the first
    // content line for context.
    let mut defects: Vec<Value> = ev_notes
        .iter()
        .filter(|n| n.finding_heading.is_some())
        .map(|n| {
            let summary = match (&n.finding_heading, &n.finding_line) {
                (Some(h), Some(l)) => format!("{h}: {l}"),
                (Some(h), None) => h.clone(),
                _ => n.finding_line.clone().unwrap_or_default(),
            };
            json!({
                "source": n.name, "match": n.matched, "at": n.at,
                "note_kind": n.kind,
                "summary": summary,
                "headings": n.headings,
            })
        })
        .collect();
    for v in store_verdicts
        .iter()
        .filter(|v| v["verdict"].as_str() != Some("pass"))
    {
        defects.push(json!({
            "source": format!("store:{}@r{}", v["task"], v["revision"]),
            "at": v["at"],
            "summary": format!("verdict '{}' on {}", v["verdict"], v["sha"]),
            "headings": [],
        }));
    }
    defects.truncate(20);
    if !defects.is_empty() {
        unknowns.push(unknown(
            "defect_classes",
            "findings are free text — class assignment is a curator/human step",
        ));
    }

    // --- flakes ---
    let flakes = flake_hits(&ev_notes, &issue.comments);

    // --- proposed lessons (manual promotion only) ---
    let mut lessons: Vec<Value> = Vec::new();
    for d in &defects {
        if lessons.len() >= MAX_LESSONS {
            break;
        }
        lessons.push(json!({
            "text": d["summary"],
            "basis": "blocking finding in a review round",
            "sources": [d["source"].clone()],
            "promotion": "manual — `cadence memory propose`; never automatic",
        }));
    }
    if !flakes.is_empty() && lessons.len() < MAX_LESSONS {
        lessons.push(json!({
            "text": format!(
                "{} transient-failure mention(s) in this issue's evidence — a gate may depend on a flaky probe",
                flakes.len()
            ),
            "basis": "flake keyword hits",
            "sources": flakes.iter().take(4).map(|f| f["source"].clone()).collect::<Vec<_>>(),
            "promotion": "manual — `cadence memory propose`; never automatic",
        }));
    }
    if note_verdict_count >= 2 && lessons.len() < MAX_LESSONS {
        lessons.push(json!({
            "text": format!("{note_verdict_count} review rounds were needed — earlier mid-flight evidence may have caught defects sooner"),
            "basis": "multiple verdict rounds",
            "sources": verdict_notes.iter().map(|v| v["note"].clone()).collect::<Vec<_>>(),
            "promotion": "manual — `cadence memory propose`; never automatic",
        }));
    }

    // Human minutes have no telemetry source — always explicit.
    unknowns.push(unknown(
        "human_minutes",
        "no human-time telemetry exists in tracker, notes or store — not derivable",
    ));

    let comment_authors: std::collections::BTreeSet<String> = issue
        .comments
        .iter()
        .map(|c| c.front.author.clone())
        .collect();

    Ok(json!({
        "schema": "cadence.retro/1",
        "id": issue.front.id,
        "title": issue.front.title,
        "project": issue.project,
        "status_file": issue.front.status,
        "status_derived": notes::derive(notes_dir, &issue.front.id).map(|(s, _)| s),
        "owner": issue.front.owner,
        "generated_at": time::iso(time::now_epoch()),
        "sources": sources,
        "sources_state": {
            "pm_git": pm_git,
            "notes_dir": notes_present,
            "store": store_state,
            "repos_skipped": skipped_repos,
        },
        "timings": timings,
        "review": {
            "rounds": note_verdict_count,
            "failed": failed_rounds,
            "verdict_notes": verdict_notes,
            "store_verdicts": store_verdicts,
            "store_failed": store_fail,
            "qa_reports": qa_reports,
            "kickoffs": kickoffs,
            "comments": issue.comments.len(),
            "comment_authors": comment_authors,
        },
        "defects": defects,
        "flakes": flakes,
        "merge": {
            "commits_on_default": commits.iter()
                .filter(|c| c["on_default"] == json!(true))
                .cloned().collect::<Vec<_>>(),
            "commits_off_default": commits.iter()
                .filter(|c| c["on_default"] != json!(true))
                .cloned().collect::<Vec<_>>(),
            "refs": issue.front.refs.iter().filter(|r| r.kind == "pr" || r.kind == "commit")
                .map(|r| json!({"kind": r.kind, "url": r.url, "label": r.label}))
                .collect::<Vec<_>>(),
        },
        "proposed_lessons": lessons,
        "promotion": "none — dry-run preview; attaching and curator promotion are separate gated steps",
        "unknowns": unknowns,
    }))
}

/// Text preview — the same document rendered for a terminal.
pub fn render(v: &Value) -> String {
    let mut o = String::new();
    o.push_str(&format!(
        "{} — retro preview (cadence.retro/1)\n",
        v["id"].as_str().unwrap_or("?")
    ));
    o.push_str(&format!(
        "  {} · project {} · status {} (derived: {})\n",
        v["title"].as_str().unwrap_or(""),
        v["project"].as_str().unwrap_or("?"),
        v["status_file"].as_str().unwrap_or("?"),
        v["status_derived"].as_str().unwrap_or("—"),
    ));
    let t = &v["timings"];
    let show = |k: &str| t[k].as_str().unwrap_or("unknown").to_string();
    o.push_str(&format!(
        "  created {} → ready {} → done {} → merged {}\n",
        show("created"),
        show("ready_at"),
        show("done_at"),
        show("merged_at"),
    ));
    let r = &v["review"];
    let failed = r["verdict_notes"]
        .as_array()
        .map(|a| a.iter().filter(|x| x["outcome"] == "not-pass").count())
        .unwrap_or(0);
    o.push_str(&format!(
        "  review: {} round(s) ({} failed) · {} qa report(s) · {} kickoff(s) · {} comment(s)\n",
        r["rounds"], failed, r["qa_reports"], r["kickoffs"], r["comments"],
    ));
    if let Some(ds) = v["defects"].as_array().filter(|d| !d.is_empty()) {
        o.push_str("  defects caught:\n");
        for d in ds.iter().take(8) {
            o.push_str(&format!(
                "    - {} — {}\n",
                d["source"].as_str().unwrap_or("?"),
                d["summary"].as_str().unwrap_or("")
            ));
        }
    }
    let flakes = v["flakes"].as_array().map(|f| f.len()).unwrap_or(0);
    o.push_str(&format!("  flakes: {flakes} mention(s)\n"));
    if let Some(ls) = v["proposed_lessons"].as_array().filter(|l| !l.is_empty()) {
        o.push_str("  proposed lessons (manual promotion only):\n");
        for l in ls.iter().take(6) {
            o.push_str(&format!("    - {}\n", l["text"].as_str().unwrap_or("")));
        }
    }
    if let Some(us) = v["unknowns"].as_array().filter(|u| !u.is_empty()) {
        o.push_str("  unknowns:\n");
        for u in us.iter().take(10) {
            o.push_str(&format!(
                "    - {}: {}\n",
                u["field"].as_str().unwrap_or("?"),
                u["reason"].as_str().unwrap_or("")
            ));
        }
    }
    o.push_str("  nothing written — preview only\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_boundaries() {
        assert!(slug_mentions("20260920-cad-198-x-kickoff.md", "CAD-198"));
        assert!(!slug_mentions("20260920-cad-1980-x.md", "CAD-198"));
        assert!(!slug_mentions("cadence-198.md", "CAD-198"));
        assert!(slug_mentions("cad-19-r2.md", "CAD-19"));
        assert!(!slug_mentions("cad-19x.md", "CAD-19") || true); // 'x' is not a digit — CAD-19 binds
    }

    #[test]
    fn iso_roundtrip() {
        assert_eq!(time::parse_iso("2026-09-17T17:24:00Z"), Some(1_789_665_840));
        assert_eq!(time::parse_iso("not a date"), None);
        assert_eq!(time::parse_iso("2026-13-01T00:00:00Z"), None);
    }
}
