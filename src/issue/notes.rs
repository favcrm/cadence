//! Agent-notes integration: a note whose header block carries
//! `Issue: <ID>` is tagged to that issue. The newest tagged note
//! derives status (`-kickoff` → doing, `-qa` → review, `-verdict` →
//! review — a verdict is a review outcome; `done` is the file field's,
//! set on merge evidence); the full set forms the issue's notes
//! chain in the drawer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::issue::time;

/// One tagged note, oldest → newest by filename timestamp.
#[derive(Clone, Debug)]
pub struct Note {
    pub name: String,
    pub path: PathBuf,
    /// `kickoff` | `qa` | `verdict` | `note` (any other suffix).
    pub kind: String,
    pub at: String,
    pub title: String,
}

/// Extract `Issue: <ID>` from a note's header block — the contiguous
/// run of title (`#`) and quote (`>`) lines at the top of the file.
/// Later body content never tags an issue.
pub fn header_issue(text: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim_end();
        if t.is_empty() {
            continue;
        }
        let probe = t
            .strip_prefix('>')
            .map(str::trim_start)
            .or_else(|| t.strip_prefix('#').map(str::trim_start));
        match probe {
            Some(inner) => {
                if let Some(rest) = inner
                    .trim_start_matches('*')
                    .trim_start()
                    .strip_prefix("Issue:")
                {
                    let id = rest.trim().trim_matches(|c: char| c == '`' || c == '*');
                    if crate::issue::model::valid_id(id) {
                        return Some(id.to_string());
                    }
                }
            }
            // First real content line ends the header block.
            None => break,
        }
    }
    None
}

/// For a `-verdict.md` note: `Some(true)` pass, `Some(false)` not-pass,
/// `None` when no verdict marker exists at all — distinct from a real
/// not-pass, so callers never misclassify an unparseable note.
/// Recognised markers: a `## Verdict` heading (answer on the next
/// content line), a `> Verdict:` / `# Verdict:` line (answer inline —
/// qa-1's convention puts it in the note title).
pub(crate) fn verdict_outcome(text: &str) -> Option<bool> {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let body = t
            .trim_start_matches('>')
            .trim_start_matches('#')
            .trim_start()
            .to_ascii_lowercase();
        if let Some(rest) = body.strip_prefix("verdict:") {
            // `Verdict: X — pass` answers inline; a bare `Verdict: X`
            // title carries no verdict word — keep scanning for a
            // `## Verdict` section instead of misreading the title.
            if word_pass(rest) {
                return Some(true);
            }
            let mentions_pass = rest
                .split(|c: char| !c.is_ascii_alphabetic())
                .any(|w| w.eq_ignore_ascii_case("pass"));
            if word_fail(rest) || mentions_pass {
                return Some(false);
            }
            continue;
        }
        if t.starts_with('#') && body == "verdict" {
            for next in &lines[i + 1..] {
                let n = next.trim();
                if !n.is_empty() {
                    return Some(word_pass(&n.to_ascii_lowercase()));
                }
            }
        }
    }
    None
}

fn word_pass(line: &str) -> bool {
    let words: Vec<String> = line
        .split(|c: char| !c.is_ascii_alphabetic())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    // "not pass"/"non-pass"/"not-pass" is a fail, not a pass.
    words.iter().enumerate().any(|(i, w)| {
        w == "pass"
            && !matches!(
                words.get(i.wrapping_sub(1)).map(|p| p.as_str()),
                Some("not") | Some("non")
            )
    })
}

/// Explicit not-pass words — used only for inline `Verdict:` answers,
/// where absence of "pass" alone can't tell "blocked" from a bare title.
fn word_fail(line: &str) -> bool {
    line.split(|c: char| !c.is_ascii_alphabetic()).any(|w| {
        matches!(
            w.to_ascii_lowercase().as_str(),
            "fail" | "failed" | "blocked" | "dropped" | "reject" | "rejected"
        )
    })
}

pub(crate) fn note_kind(name: &str) -> String {
    name.strip_suffix(".md")
        .and_then(|n| n.rsplit('-').next())
        .map(|s| match s {
            "kickoff" | "qa" | "verdict" => s.to_string(),
            _ => "note".to_string(),
        })
        .unwrap_or_else(|| "note".to_string())
}

/// One `.md` note from a directory entry, with the issue its header
/// tags — `None` for non-notes and untagged notes.
fn read_note(entry: &std::fs::DirEntry) -> Option<(String, Note)> {
    let name = entry.file_name().to_string_lossy().to_string();
    if !name.ends_with(".md") {
        return None;
    }
    let path = entry.path();
    let text = std::fs::read_to_string(&path).ok()?;
    let id = header_issue(&text)?;
    let note = Note {
        at: time::note_name_to_iso(&name).unwrap_or_default(),
        kind: note_kind(&name),
        title: text
            .lines()
            .find(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches('#').trim().to_string())
            .unwrap_or_default(),
        name,
        path,
    };
    Some((id, note))
}

/// Every note in `notes_dir` tagged `Issue: <id>`, oldest first.
/// Filenames carry the UTC timestamp so name order is chronological.
pub fn chain(notes_dir: &Path, id: &str) -> Vec<Note> {
    let mut notes = Vec::new();
    let Ok(entries) = std::fs::read_dir(notes_dir) else {
        return notes;
    };
    for entry in entries.flatten() {
        if let Some((tagged, note)) = read_note(&entry) {
            if tagged == id {
                notes.push(note);
            }
        }
    }
    notes.sort_by(|a, b| a.name.cmp(&b.name));
    notes
}

/// Every tagged note in `notes_dir` grouped by issue id, each chain
/// oldest first — one directory walk and one read per note, where a
/// per-issue [`chain`] re-reads the whole directory for every issue
/// (a board render over hundreds of issues was seconds of I/O).
pub fn index(notes_dir: &Path) -> HashMap<String, Vec<Note>> {
    let mut by_issue: HashMap<String, Vec<Note>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(notes_dir) else {
        return by_issue;
    };
    for entry in entries.flatten() {
        if let Some((id, note)) = read_note(&entry) {
            by_issue.entry(id).or_default().push(note);
        }
    }
    for notes in by_issue.values_mut() {
        notes.sort_by(|a, b| a.name.cmp(&b.name));
    }
    by_issue
}

/// The derived status from the newest tagged note, if any.
/// `Some((status, note))`; `None` → fall through to the file field.
/// M3 job state will slot in ahead of this step — that seam is
/// [`crate::issue::board::derive_status`].
pub fn derive(notes_dir: &Path, id: &str) -> Option<(&'static str, Note)> {
    let latest = chain(notes_dir, id).into_iter().next_back()?;
    let status = derive_from(&latest)?;
    Some((status, latest))
}

/// The status one newest note derives — `None` for a plain note.
/// A verdict is a review outcome (`review`), pass or not: `done`
/// needs merge evidence, which reconcile (CAD-754) and
/// `mark_done_on_merge` (CAD-449) write to the file field.
pub fn derive_from(latest: &Note) -> Option<&'static str> {
    Some(match latest.kind.as_str() {
        "kickoff" => "doing",
        "qa" => "review",
        "verdict" => "review",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_issue_line() {
        let text = "# Kickoff: thing\n> Session: `abc12`\n> Issue: `CAD-22`\n> From: `x`\n\n## Body\nIssue: CAD-99 is ignored\n";
        assert_eq!(header_issue(text).as_deref(), Some("CAD-22"));
        assert_eq!(header_issue("no header here").as_deref(), None);
        assert_eq!(
            header_issue("# t\n## also header\n> Issue: SPL-4\n").as_deref(),
            Some("SPL-4")
        );
        // A bare `Issue:` line is body text, not a header tag — prose
        // like "see Issue: CAD-9" must never tag the note.
        assert_eq!(header_issue("# t\n\nIssue: SPL-4\n").as_deref(), None);
    }

    /// CAD-823: a verdict note is a review outcome, never delivery —
    /// pass or not it derives `review`; `done` is the file field's,
    /// set by reconcile/mark-done-on-merge on merge evidence.
    #[test]
    fn derive_from_maps_note_kinds() {
        let dir = tempfile::TempDir::new().unwrap();
        let note = |kind: &str, name: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, "## Verdict\n**Pass.**\n").unwrap();
            Note {
                name: name.to_string(),
                path,
                kind: kind.to_string(),
                at: String::new(),
                title: String::new(),
            }
        };
        assert_eq!(derive_from(&note("kickoff", "a-kickoff.md")), Some("doing"));
        assert_eq!(derive_from(&note("qa", "b-qa.md")), Some("review"));
        assert_eq!(
            derive_from(&note("verdict", "c-verdict.md")),
            Some("review"),
            "a PASS verdict is passed review, not delivered"
        );
        assert_eq!(derive_from(&note("note", "d-note.md")), None);
    }

    /// A not-passing verdict derives `review` the same.
    #[test]
    fn derive_from_revise_verdict_is_review() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("20260929-120000-x-verdict.md");
        std::fs::write(&path, "# Verdict: x\n\n## Verdict\n**Revise.**\n").unwrap();
        let note = Note {
            name: "20260929-120000-x-verdict.md".to_string(),
            path,
            kind: "verdict".to_string(),
            at: String::new(),
            title: String::new(),
        };
        assert_eq!(derive_from(&note), Some("review"));
    }

    #[test]
    fn verdict_outcome_tri_state() {
        // Title-carried verdict — qa-1's convention.
        assert_eq!(
            verdict_outcome("# Verdict: PR #86 — CAD-198 census — pass\n\nBody.\n"),
            Some(true)
        );
        assert_eq!(
            verdict_outcome("# Verdict: CAD-1 — blocked\n\nBody.\n"),
            Some(false)
        );
        // A bare `Verdict:` title is not an answer — scan on.
        assert_eq!(
            verdict_outcome("# Verdict: x\n\n## Verdict\n**Pass.**\n"),
            Some(true)
        );
        // Negated pass is a fail, not a pass.
        assert_eq!(verdict_outcome("> Verdict: not pass\n"), Some(false));
        assert_eq!(verdict_outcome("> Verdict: not-pass\n"), Some(false));
        // No verdict marker at all → None, distinct from not-pass.
        assert_eq!(verdict_outcome("# Note\n\nno verdict here\n"), None);
    }
}
