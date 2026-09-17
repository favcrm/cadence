//! Agent-notes integration: a note whose header block carries
//! `Issue: <ID>` is tagged to that issue. The newest tagged note
//! derives status (`-kickoff` → doing, `-qa` → review, `-verdict` →
//! done on pass else review); the full set forms the issue's notes
//! chain in the drawer.

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

/// For a `-verdict.md` note: the first content line after a
/// `## Verdict`-style heading decides pass vs not.
fn verdict_passes(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let is_verdict_heading = t.starts_with('#')
            && t.trim_start_matches('#')
                .trim_start()
                .eq_ignore_ascii_case("verdict")
            || t.trim_start_matches('>')
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("verdict:");
        if !is_verdict_heading {
            continue;
        }
        // `> Verdict: pass` carries the answer on the same line.
        if let Some(rest) = t.to_ascii_lowercase().split("verdict:").nth(1) {
            return word_pass(rest);
        }
        for next in &lines[i + 1..] {
            let n = next.trim();
            if !n.is_empty() {
                return word_pass(&n.to_ascii_lowercase());
            }
        }
    }
    false
}

fn word_pass(line: &str) -> bool {
    line.split(|c: char| !c.is_ascii_alphabetic())
        .any(|w| w.eq_ignore_ascii_case("pass"))
}

fn note_kind(name: &str) -> String {
    name.strip_suffix(".md")
        .and_then(|n| n.rsplit('-').next())
        .map(|s| match s {
            "kickoff" | "qa" | "verdict" => s.to_string(),
            _ => "note".to_string(),
        })
        .unwrap_or_else(|| "note".to_string())
}

/// Every note in `notes_dir` tagged `Issue: <id>`, oldest first.
/// Filenames carry the UTC timestamp so name order is chronological.
pub fn chain(notes_dir: &Path, id: &str) -> Vec<Note> {
    let mut notes = Vec::new();
    let Ok(entries) = std::fs::read_dir(notes_dir) else {
        return notes;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        let path = entry.path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if header_issue(&text).as_deref() != Some(id) {
            continue;
        }
        notes.push(Note {
            at: time::note_name_to_iso(&name).unwrap_or_default(),
            kind: note_kind(&name),
            title: text
                .lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .unwrap_or_default(),
            name,
            path,
        });
    }
    notes.sort_by(|a, b| a.name.cmp(&b.name));
    notes
}

/// The derived status from the newest tagged note, if any.
/// `Some((status, note))`; `None` → fall through to the file field.
/// M3 job state will slot in ahead of this step — that seam is
/// [`crate::issue::board::derive_status`].
pub fn derive(notes_dir: &Path, id: &str) -> Option<(&'static str, Note)> {
    let latest = chain(notes_dir, id).into_iter().next_back()?;
    let status = match latest.kind.as_str() {
        "kickoff" => "doing",
        "qa" => "review",
        "verdict" => {
            let text = std::fs::read_to_string(&latest.path).unwrap_or_default();
            if verdict_passes(&text) {
                "done"
            } else {
                "review"
            }
        }
        _ => return None,
    };
    Some((status, latest))
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

    #[test]
    fn verdict_words() {
        let pass = "# Verdict: x\n> Session: `s`\n\n## Verdict\n**Pass.** Ship it.\n";
        assert!(verdict_passes(pass));
        let dropped = "# Verdict: x\n\n## Verdict\n**Dropped.** No.\n";
        assert!(!verdict_passes(dropped));
        let inline = "# v\n> Verdict: pass\n";
        assert!(verdict_passes(inline));
    }
}
