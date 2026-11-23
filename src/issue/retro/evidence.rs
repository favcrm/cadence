//! Note loading, daemon-store verdict querying, and flake detection for
//! `issue::retro`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use serde_json::{json, Value};

use crate::issue::{board, notes, time};

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
pub(super) const MAX_LESSONS: usize = 10;
const SNIPPET_LEN: usize = 200;

pub(super) fn unknown(field: &str, reason: impl Into<String>) -> Value {
    json!({"field": field, "reason": reason.into()})
}

/// Does `name` carry the id slug (`cad-198`) at a token boundary?
/// Filename matches bind only when the note did not tag itself —
/// `CAD-19` must not bind to `cad-1980`.
pub(super) fn slug_mentions(name: &str, id: &str) -> bool {
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
pub(super) struct EvNote {
    /// Bare filename — the source reference.
    pub(super) name: String,
    /// `header` (canonical `Issue:` tag) | `filename` (slug only).
    pub(super) matched: &'static str,
    pub(super) kind: String,
    pub(super) at: String,
    pub(super) title: String,
    /// `Some(pass)` for verdict notes.
    pub(super) verdict: Option<bool>,
    /// `##`/`###` heading lines, bounded.
    pub(super) headings: Vec<String>,
    /// The first `##`-level finding-section heading, if any.
    pub(super) finding_heading: Option<String>,
    /// First non-metadata line under that heading.
    pub(super) finding_line: Option<String>,
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
pub(super) fn load_notes(notes_dir: &Path, id: &str) -> Option<Vec<EvNote>> {
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
pub(super) fn first_set(log: &[Value], to: &str) -> Option<String> {
    log.iter()
        .rev()
        .find(|e| e["kind"] == "set" && e["fields"]["status"].as_str() == Some(to))
        .and_then(|e| e["at"].as_str().map(String::from))
}

/// Daemon-store verdict evidence joined on `jobs.issue_id`, opened
/// `SQLITE_OPEN_READ_ONLY`. `absent` and `unreadable` are distinct
/// outcomes; neither is fatal.
pub(super) fn store_rows(state_dir: &Path, id: &str) -> (Value, Vec<Value>, Option<String>) {
    let path = state_dir.join("cadence.sqlite3");
    if !path.exists() {
        return (
            json!("absent"),
            Vec::new(),
            Some("no daemon store — jobs/tasks/verdicts corroboration unavailable".into()),
        );
    }
    let conn = match crate::store::open_read_only(&path) {
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
pub(super) fn flake_hits(notes: &[EvNote], comments: &[board::Comment]) -> Vec<Value> {
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
