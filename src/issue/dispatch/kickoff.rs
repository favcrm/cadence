//! Kickoff body rendering, acceptance-listing serialization/parsing, and pre-dispatch validation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use crate::issue::parse::AcceptanceItem;

mod jobs;

pub(super) use jobs::{check_body, check_job_kickoff, job_prototype, plain_body, plain_kickoff};

/// The fixed single-line kickoff body — note path, issue id, summary
/// or title, the worktree/branch/base, the trailer and the return
/// address. Built verbatim: a title or `--summary` carrying control
/// chars produces a violating body and `check_body` refuses rather
/// than laundering it into something the operator didn't write.
///
/// CAD-468: an explicitly-reported endpoint's kickoff also carries the
/// exact `cadence message result` command — the worker that missed or
/// never read its briefing still has the contract in the message that
/// opened its turn. Turn-result endpoints finish by their result text
/// and never run it, so theirs is omitted.
#[allow(clippy::too_many_arguments)]
pub(super) fn kickoff_body(
    issue: &str,
    title: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
    provider: &str,
    endpoint_kind: &str,
) -> String {
    let guidance = crate::briefing::TASK_GUIDANCE;
    let sha7: String = base_sha.chars().take(7).collect();
    let explicit = crate::adapter::registry::report_hint(provider, endpoint_kind)
        == crate::adapter::registry::Reporting::Explicit;
    let report = if explicit {
        " Report: `cadence message result <id> --token <turn_id> --report <done-report-file> \
         --text '<summary>'` (`cadence self` prints the id and token). That one call files the \
         done report once, records the `Report:` link the review loop follows, and completes the \
         turn — do not also run `cadence report file` for it."
    } else {
        ""
    };
    let completion = if explicit {
        " End its text with a final `SHA: <full committed head>` line; that text is the contract."
            .to_string()
    } else {
        format!(
            " For implementation completion, file `cadence report file --task {issue} --kind done \
             --file <f>` once with frontmatter `sha: <full committed head>` and `pr: <PR URL>`, \
             then include the exact `Report: {issue}/reports/<name>` line the CLI printed in your \
             final answer, ending it with `SHA: <same full head>`."
        )
    };
    format!(
        "read {} — {issue}: {title}. Your worktree exists: {} (branch \
         {branch}, base {sha7}). Commit trailer: Issue: {issue}. Follow the repo's \
         integration and review rules; reply to {reply_to}.{guidance}{report}{completion}",
        note.display(),
        wt_dir.display()
    )
}

/// CAD-159: an issue's acceptance items on one line; `None` when there
/// are none. Always the whole list — CAD-160 removed the "N items, too
/// long to inline" pointer, since criteria are never dropped.
///
/// CAD-300: each item is numbered and its text is a JSON string
/// literal — `1) [ ] "a"; 2) [x] "b"` — so text that looks like the
/// listing's own syntax (`; `, `[x]`, `2)`) stays inside its quotes
/// and cannot read as another item or a checked box.
/// [`parse_acceptance_listing`] reads it back exactly. Quoting escapes
/// every control character and U+2028/U+2029, so the result can ride a
/// single-line pty kickoff without losing them.
pub(crate) fn acceptance_listing(items: &[AcceptanceItem]) -> Option<String> {
    (!items.is_empty()).then(|| inline_listing(items))
}

/// Render the approved Markdown as one anchored, unambiguous scope.
pub(crate) fn intake_acceptance(scope: &str) -> Result<String> {
    let scope = serde_json::to_string(scope).map_err(|e| Error::internal(e.to_string()))?;
    Ok(format!(
        "Approved issue acceptance Markdown (including qualifiers): {scope}"
    ))
}

pub(crate) fn with_independent_acceptance(
    mut rendered: String,
    independent: Option<&str>,
) -> Result<String> {
    if let Some(recorded) = independent.filter(|s| !s.trim().is_empty()) {
        rendered.push_str(". Independent task acceptance (where compatible): ");
        rendered.push_str(
            &serde_json::to_string(recorded).map_err(|e| Error::internal(e.to_string()))?,
        );
    }
    Ok(rendered)
}

/// The CAD-300 listing of `items` at any length —
/// `1) [ ] "a"; 2) [x] "b"`, numbered from 1.
pub(crate) fn inline_listing(items: &[AcceptanceItem]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mark = if item.checked { 'x' } else { ' ' };
            format!("{}) [{mark}] {}", i + 1, quote_item(&item.text))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// CAD-160: the still-unchecked criteria in a task's stored
/// acceptance. A whole CAD-300 listing yields its `[ ]` items; any
/// other non-blank text (a free-form `--accept`) is one unchecked item.
pub(crate) fn outstanding_items(acceptance: Option<&str>) -> Vec<AcceptanceItem> {
    let Some(text) = acceptance.map(str::trim).filter(|t| !t.is_empty()) else {
        return vec![];
    };
    match parse_acceptance_listing(text) {
        Some((items, "")) => items.into_iter().filter(|i| !i.checked).collect(),
        _ => vec![AcceptanceItem {
            text: text.to_string(),
            checked: false,
        }],
    }
}

/// `text` as a JSON string literal that is also safe on one pty line:
/// `"` and `\` are escaped as JSON requires, tab as `\t`, and every
/// other control character (C0, DEL, C1) and the line/paragraph
/// separators U+2028/U+2029 as `\uXXXX`. All of those are in the BMP,
/// so four hex digits always suffice.
fn quote_item(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// CAD-300: read an [`acceptance_listing`] back — the numbered,
/// JSON-quoted items at the start of `s` — returning them and the rest
/// of `s` (the kickoff's `.` onwards). `None` when `s` does not start
/// with item `1)`, or an item is malformed: a numbering gap, a checkbox
/// other than `[ ]`/`[x]`, or text that is not a JSON string literal.
pub fn parse_acceptance_listing(s: &str) -> Option<(Vec<AcceptanceItem>, &str)> {
    let mut items = vec![];
    let mut rest = s;
    loop {
        rest = rest.strip_prefix(&format!("{}) ", items.len() + 1))?;
        let checked = match rest.get(..4)? {
            "[x] " => true,
            "[ ] " => false,
            _ => return None,
        };
        rest = &rest[4..];
        if !rest.starts_with('"') {
            return None;
        }
        let mut quoted = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let text = quoted.next()?.ok()?;
        rest = &rest[quoted.byte_offset()..];
        items.push(AcceptanceItem { text, checked });
        match rest.strip_prefix("; ") {
            Some(next) if next.starts_with(&format!("{}) ", items.len() + 1)) => rest = next,
            _ => return Some((items, rest)),
        }
    }
}

/// CAD-159: the warning for an issue with no acceptance items. The PM
/// decided (2026-09-23, ADR-0002 §8.3) to warn and still dispatch
/// until live issues are backfilled; refusal is a later ticket.
pub(super) fn acceptance_warning(issue: &str, dispatched: bool) -> String {
    let outcome = if dispatched {
        "Dispatched anyway."
    } else {
        "Not re-sent: this issue's kickoff is already in flight."
    };
    format!(
        "{issue} has no acceptance criteria (its ## Acceptance section has no \
         checklist items; a bare `- [ ]` does not count) — add them with \
         `cadence issue acceptance {issue} --from <file>`. {outcome}"
    )
}

/// The plain-path kickoff with every one of the issue's acceptance
/// items appended as ` Acceptance: <listing>.` — never a pointer, never
/// cut (CAD-160). [`plain_kickoff`] fits the result to the pty ceiling.
pub(super) fn with_acceptance(body: String, items: &[AcceptanceItem]) -> String {
    match acceptance_listing(items) {
        Some(listing) => format!("{body} Acceptance: {listing}."),
        None => body,
    }
}
