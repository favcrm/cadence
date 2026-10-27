//! Notes.

use super::*;

/// One note's extracted evidence. Only the fields the audit renders.
/// CAD-918: delegated approvals read notes through this same parser.
#[derive(Debug, Default)]
pub(crate) struct Note {
    pub(crate) path: PathBuf,
    /// `verdict` | `ops-merge` | `other` — from the filename.
    pub(crate) kind: String,
    /// `From:` identity.
    pub(crate) from: Option<String>,
    /// Full 40-hex tokens seen in the note.
    pub(super) shas: Vec<String>,
    /// `#NN` PR references seen in the note.
    pub(crate) prs: Vec<u64>,
    /// The `Issue:` header — binds a verdict to one tracker issue.
    pub(crate) issue: Option<String>,
    /// The sha a `head …` line names — the verdict's reviewed head.
    pub(crate) head_sha: Option<String>,
    pub(crate) verdict: Option<String>,
    /// CAD-959: the `## Verdict` section's word and the inline `Verdict:`
    /// line's word, kept apart so a caller can see them disagree.
    pub(crate) verdict_section: Option<String>,
    pub(crate) verdict_inline: Option<String>,
    pub(crate) class: Option<String>,
    pub(super) trigger: Option<String>,
    pub(super) auditor_check: Option<String>,
    pub(super) gates: Vec<String>,
    pub(super) residue: Vec<String>,
    /// Post-merge outcome lines (`tree`, `smoke`, `restart`, `revert`).
    pub(super) outcomes: HashMap<String, String>,
    /// Compact gate summary: suite result, stress runs, disclosed flakes.
    pub(super) gate_suite: Option<String>,
    pub(super) gate_stress: Option<String>,
    pub(super) gate_flakes: Option<String>,
}

/// One compact gate fragment: `N passed/M failed` when the line carries
/// counts, else the line trimmed of markdown and truncated at 80 chars.
pub(super) fn gate_fragment(line: &str) -> String {
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
pub(super) fn gate_is_stress(lower: &str) -> bool {
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
pub(super) fn note_index(dir: &Path) -> Option<Vec<Note>> {
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

pub(super) fn parse_note(path: &Path, name: &str, text: &str) -> Note {
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
    let mut past_header = false;
    for line in text.lines() {
        let t = line.trim();
        let lower = t.to_lowercase();
        // `From:` comes from the header, before the first `## ` heading,
        // and the first one wins: a body line must not rename the reviewer.
        if t.starts_with("## ") {
            past_header = true;
        }
        if note.from.is_none() && !past_header {
            if let Some(rest) = t
                .strip_prefix("From:")
                .or_else(|| t.strip_prefix("> From:"))
            {
                note.from = Some(rest.trim().trim_matches('`').to_string());
            }
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
            note.verdict_inline = Some(
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
            if ["auto", "notify", "delegated", "human"].contains(&class.as_str()) {
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
    // The `## Verdict` section (first non-empty line) outranks an inline
    // `Verdict:` line: a body line must not override the verdict section.
    let mut seen = false;
    for line in text.lines() {
        let l = line.trim();
        if l.to_lowercase().starts_with("## verdict") {
            seen = true;
            continue;
        }
        if seen && !l.is_empty() {
            note.verdict_section = Some(
                l.trim_matches('`')
                    .split([' ', '—', '-'])
                    .next()
                    .unwrap_or("")
                    .to_string(),
            );
            break;
        }
    }
    note.verdict = note
        .verdict_section
        .clone()
        .or_else(|| note.verdict_inline.clone());
    note
}

/// Hex tokens ≥7 chars in a blob. In general context all-digit tokens
/// are excluded — note filenames carry `YYYYMMDD` stamps that are not
/// SHAs — but all-letter tokens stay: `cabbebe` is a real abbrev. In a
/// `head …` context (`head_sha`) all-digit tokens stay too: a real
/// abbrev can be all digits (`5428215`).
pub(super) fn hex_shas(text: &str) -> Vec<String> {
    hex_shas_ctx(text, false)
}

pub(super) fn hex_shas_ctx(text: &str, head_ctx: bool) -> Vec<String> {
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
pub(super) fn pr_refs(text: &str) -> Vec<u64> {
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

pub(super) fn sha_hit(note: &Note, sha: &str) -> bool {
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
pub(super) fn verdict_matches(note: &Note, row: &Row) -> bool {
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
pub(super) fn stale_verdict<'a>(notes: &'a [Note], row: &Row) -> Option<&'a Note> {
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
pub(super) fn ops_matches(note: &Note, row: &Row) -> bool {
    sha_hit(note, &row.merge_sha) || row.landed_head.as_deref().is_some_and(|h| sha_hit(note, h))
}

pub(super) fn enrich_notes(notes: Option<&[Note]>, dir: &Path, row: &mut Row) {
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
pub(super) fn finalize_unknowns(row: &mut Row) {
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
