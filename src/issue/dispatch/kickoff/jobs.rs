//! Plain and job kickoff assembly, job prototypes, and pre-dispatch body validation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use serde_json::Value;

use super::{acceptance_listing, kickoff_body, with_acceptance};
use crate::adapter::pty;
use crate::error::{Error, Result};
use crate::issue::model::Front;
use crate::issue::parse::AcceptanceItem;
use crate::store;

/// CAD-160: the plain kickoff fitted to the pty ceiling. Prose gives
/// way — the title (or `--summary`) is shortened and ends in `…` — and
/// the acceptance items never do. When they cannot fit even with the
/// title cut to nothing, the dispatch refuses naming the ceiling and
/// the note; it runs before `issue start`, so nothing is created.
#[allow(clippy::too_many_arguments)]
pub(in super::super) fn plain_kickoff(
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
pub(in super::super) fn plain_body(
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
pub(in super::super) fn job_prototype(
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
pub(in super::super) fn check_job_kickoff(
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
pub(in super::super) fn check_body(body: &str, provider: &str) -> Result<()> {
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
