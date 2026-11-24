//! Kickoff body rendering, acceptance-listing serialization/parsing, and pre-dispatch validation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use serde_json::Value;

use crate::adapter::pty;
use crate::error::{Error, Result};
use crate::issue::model::Front;
use crate::issue::parse::AcceptanceItem;
use crate::store;

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
pub fn acceptance_listing(items: &[AcceptanceItem]) -> Option<String> {
    (!items.is_empty()).then(|| inline_listing(items))
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

/// CAD-160: the plain kickoff fitted to the pty ceiling. Prose gives
/// way — the title (or `--summary`) is shortened and ends in `…` — and
/// the acceptance items never do. When they cannot fit even with the
/// title cut to nothing, the dispatch refuses naming the ceiling and
/// the note; it runs before `issue start`, so nothing is created.
#[allow(clippy::too_many_arguments)]
pub(super) fn plain_kickoff(
    issue: &str,
    title: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
    items: &[AcceptanceItem],
    provider: &str,
    endpoint_kind: &str,
) -> Result<String> {
    const CUT: &str = "…";
    let build = |title: &str| {
        with_acceptance(
            kickoff_body(
                issue,
                title,
                note,
                wt_dir,
                branch,
                base_sha,
                reply_to,
                provider,
                endpoint_kind,
            ),
            items,
        )
    };
    let full = build(title);
    let over = full.len().saturating_sub(pty::MAX_BODY);
    if over == 0 {
        return Ok(full);
    }
    if let Some(keep) = title.len().checked_sub(over + CUT.len()) {
        let mut end = keep;
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        return Ok(build(&format!("{}{CUT}", &title[..end])));
    }
    let criteria = acceptance_listing(items).unwrap_or_default();
    Err(Error::rejected(format!(
        "{issue}'s acceptance criteria ({} bytes) do not fit the {}-char pty kickoff \
         ceiling even with the title cut — criteria are never truncated or dropped \
         (CAD-160). Shorten them (`cadence issue acceptance {issue} --from <file>`) \
         and keep the detail in the note {}. Nothing was created or queued.",
        criteria.len(),
        pty::MAX_BODY,
        note.display()
    )))
}

/// The plain kickoff with every acceptance item appended, checked against
/// the pty rules. Built by the caller for the legacy opt-out path, or by
/// the context guard's lazy factory for an opted-in one — never both.
#[allow(clippy::too_many_arguments)]
pub(super) fn plain_body(
    front: &Front,
    summary: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
    items: &[AcceptanceItem],
    provider: &str,
    endpoint_kind: &str,
) -> Result<String> {
    let body = plain_kickoff(
        &front.id,
        summary,
        note,
        wt_dir,
        branch,
        base_sha,
        reply_to,
        items,
        provider,
        endpoint_kind,
    )?;
    check_body(&body, provider)?;
    Ok(body)
}

/// The placeholder job/task `issue start --job` is about to create, with
/// the exact spec path, scope, criteria and id lengths the daemon mints
/// (`job-` + 8 hex, `-t1`, a 32-hex message id). Both the legacy kickoff
/// size check and the pre-admission context guard use it.
pub(super) fn job_prototype(
    issue: &str,
    items: &[AcceptanceItem],
    spec: &Path,
    wt_name: &str,
    branch: &str,
    base_sha: &str,
) -> (store::Job, store::Task) {
    // `issue start --job` records the canonical spec path on the job.
    let spec = spec.canonicalize().unwrap_or_else(|_| spec.to_path_buf());
    let job_id = "job-00000000";
    let job = store::Job {
        id: job_id.to_string(),
        title: None,
        spec_path: spec.to_string_lossy().into_owned(),
        spec_sha256: None,
        pm_alias: String::new(),
        issue_id: Some(issue.to_string()),
        repo: None,
        base_ref: None,
        state: "open".to_string(),
        max_revisions: 0,
        stall_secs: None,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    let task = store::Task {
        id: format!("{job_id}-t1"),
        job_id: job_id.to_string(),
        title: None,
        role: "worker".to_string(),
        assignee: None,
        spec_path: None,
        acceptance: acceptance_listing(items),
        worktree: Some(wt_name.to_string()),
        branch: Some(branch.to_string()),
        base_sha: Some(base_sha.to_string()),
        head_sha: None,
        state: "draft".to_string(),
        revision: 0,
        dispatch_message: None,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    (job, task)
}

/// CAD-160: a `--job` dispatch whose kickoff cannot carry its
/// acceptance items whole is refused before `issue start` creates a
/// worktree, branch or job. The check builds the legacy kickoff `job
/// dispatch` will send — same spec path, scope, criteria and report
/// contract, for the assignee's own endpoint and ceiling (48000 for a
/// Devin cloud session) — with placeholders the length of what the daemon
/// mints. `agent` is the assignee's `agent_show` row. An opted-in
/// kickoff's exact body and combined cap are checked by [`job_context`]
/// instead.
pub(super) fn check_job_kickoff(
    issue: &str,
    items: &[AcceptanceItem],
    spec: &Path,
    wt_name: &str,
    branch: &str,
    base_sha: &str,
    agent: &Value,
) -> Result<()> {
    let (job, task) = job_prototype(issue, items, spec, wt_name, branch, base_sha);
    let provider = agent["provider"].as_str().unwrap_or_default();
    let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
    // Legacy preview: no pinned context, so the acceptance rides the body.
    let Err(_) = store::job_kickoff(&job, &task, 1, &"0".repeat(32), provider, kind, false) else {
        return Ok(());
    };
    Err(Error::rejected(format!(
        "{issue}'s acceptance criteria ({} bytes) do not fit the {}-char kickoff ceiling \
         beside its fixed fields — criteria are never truncated or dropped (CAD-160). \
         Shorten them (`cadence issue acceptance {issue} --from <file>`) and keep the \
         detail in the spec file {}. Nothing was created or queued.",
        acceptance_listing(items).unwrap_or_default().len(),
        store::kickoff_ceiling(provider, kind),
        job.spec_path,
    )))
}

/// The same rules the pty endpoint enforces pre-write: 1–4000 chars,
/// no control characters (single line), and no leading character the
/// provider's TUI treats as a command. Checked here so a bad body
/// refuses before `issue start` creates anything.
pub(super) fn check_body(body: &str, provider: &str) -> Result<()> {
    if body.is_empty() || body.len() > pty::MAX_BODY {
        return Err(Error::rejected("Dispatch body must be 1–4000 characters"));
    }
    if pty::has_control_chars(body) {
        return Err(Error::rejected(
            "Dispatch body must be a single line without control characters",
        ));
    }
    if let Some(prefix) = body.trim_start().chars().next() {
        if pty::forbidden_prefixes(provider).contains(&prefix) {
            return Err(Error::rejected(format!(
                "Dispatch body starts with '{prefix}', which {provider} \
                 treats as a command or mode switch — refusing to send it"
            )));
        }
    }
    Ok(())
}
