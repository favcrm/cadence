//! Kickoff bodies: SHA trailer, spec inlining, host-path scrubbing.

use crate::adapter::{pty, registry};
use crate::error::{Error, Result};
use sha2::{Digest, Sha256};

use super::agents::Agent;
use super::messages::ENQUEUE_BYTES;
use super::plans::{Job, Task};
use super::take_bytes;

/// A reported commit must be an explicit hex object id — 40 hex
/// (SHA-1) or 64 (SHA-256 repos). Never inferred, never partial.
pub fn check_commit_sha(sha: &str) -> Result<String> {
    let ok = matches!(sha.len(), 40 | 64) && sha.chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        Ok(sha.to_ascii_lowercase())
    } else {
        Err(Error::rejected(format!(
            "'{sha}' is not a commit SHA — expected 40 or 64 hex characters"
        )))
    }
}

/// The `SHA: <hex>` convention for managed endpoints (A3): the last
/// matching line of a result text is the reported commit. Managed
/// `codex`/`claude`/`fake` turns complete from final text and never
/// call `message result --sha`, so the kickoff asks the agent to end
/// its answer with this line. The LAST line wins — a worker discussing
/// SHAs mid-answer cannot shadow the trailer it ends with.
pub(crate) fn last_sha_line(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let hex = line
            .trim()
            .strip_prefix("SHA:")
            .or_else(|| line.trim().strip_prefix("sha:"))?
            .trim();
        check_commit_sha(hex).ok()
    })
}

/// The explicit-endpoint tail the pty render probe can tell apart:
/// a bounded digest of the durable message id, appended last so the
/// body's final characters differ across dispatches even when every
/// other field is shared boilerplate. A fixed 32-hex digest (not the
/// raw id) keeps the suffix bounded for `--message` override ids of
/// arbitrary length; the same id always mints the same suffix.
pub(super) fn flatten_controls(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn path_boundary(prev: Option<u8>) -> bool {
    matches!(
        prev,
        None | Some(b' ' | b'\n' | b'\t' | b'\r' | b'`' | b'"' | b'(')
    )
}

/// A path is machine-local when it is `~/...` or an absolute path whose
/// first segment is not an API version (`/v3/...`, `/v3beta1/...`).
/// That drops `/home`, `/tmp`, `/var`, `/etc`, and other host paths
/// such as `/secret/...`, and keeps a backticked Devin API path.
fn local_path(token: &str) -> bool {
    if token.starts_with("~/") || token == "~" {
        return true;
    }
    if !token.starts_with('/') {
        return false;
    }
    let segment = token[1..].split(['/', '?', '#']).next().unwrap_or("");
    let api = segment.len() >= 2
        && segment.as_bytes()[0] == b'v'
        && segment.as_bytes()[1].is_ascii_digit();
    !api
}

/// Drop machine-local paths so a cloud session is not pointed at a host
/// file. Paths after a space, newline, backtick, quote, or `(` are
/// included. API paths (`/v3/organizations/...`) and `https://` URLs
/// stay.
pub fn omit_host_paths(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'/' {
            let ch = text[index..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            index += ch.len_utf8();
            continue;
        }
        let prev = if index == 0 {
            None
        } else {
            Some(bytes[index - 1])
        };
        let tilde = prev == Some(b'~')
            && path_boundary(if index >= 2 {
                Some(bytes[index - 2])
            } else {
                None
            });
        let url = prev == Some(b':');
        let start = if tilde { index - 1 } else { index };
        let token = text[start..]
            .split_whitespace()
            .next()
            .unwrap_or(&text[start..]);
        if (tilde || (path_boundary(prev) && !url)) && local_path(token) {
            if tilde {
                out.pop();
            }
            out.push_str("(omitted)");
            index = start + token.len();
        } else {
            out.push('/');
            index += 1;
        }
    }
    out
}

const SPEC_NOTE: &str = "… (spec text truncated; the inlined copy is incomplete)";

/// The report contract. Reserved before any cut so a long acceptance
/// or spec cannot chop this suffix.
const SHA_TRAILER: &str = " Report when done: end your final answer with a one-line summary \
followed by a last line `SHA: <40-hex>` naming the commit you produced — the daemon reads \
that line as the reported revision. Do not report a SHA you have not committed.";

/// Cloud sessions cannot read the host spec path or run `cadence self`.
/// The spec text is inlined and the `SHA:` trailer stays the report contract.
/// The operator exit for a held Devin cloud turn. The held message
/// stays `unknown`: a daemon restart or `agent stop` during the hold
/// fences the worker (resume is refused), and a stop does not archive
/// the session.
pub(super) fn cloud_hold_exit(message: &str, alias: &str, session: &str) -> String {
    format!(
        "The held message `{message}` stays unknown. If the daemon restarts or the agent \
         is stopped before a poll settles it, that message fences the worker until it is \
         reconciled; the session is not archived and nothing is replayed. To settle it by \
         hand, inspect the Devin session ({session}), then run `cadence message reconcile {message} \
         --status <completed|failed|interrupted>` (add `--sha <40-hex>` for a completed \
         commit) or, after a restart or stop has fenced the worker, `cadence agent unfence \
         {alias} --status <completed|failed|interrupted>`, choosing the status from what the session shows. \
         `cadence agent resume {alias}` is refused while the message is unknown."
    )
}

pub(super) fn cloud_kickoff_body(job: &Job, task: &Task, revision: i64) -> Result<String> {
    let spec = task.spec_path.as_deref().unwrap_or(job.spec_path.as_str());
    let raw = std::fs::read_to_string(spec)
        .unwrap_or_else(|_| "(spec text was not available to inline)".to_string());
    let cleaned = omit_host_paths(&flatten_controls(&raw));
    let mut scope = String::new();
    if let Some(branch) = &task.branch {
        scope.push_str(&format!(" branch {}", flatten_controls(branch)));
    }
    if let Some(base) = &task.base_sha {
        scope.push_str(&format!(" base {}", flatten_controls(base)));
    }
    if !scope.is_empty() {
        scope = format!(" Scope:{scope}.");
    }
    let acceptance = task
        .acceptance
        .as_deref()
        .map(|text| format!(" Acceptance: {}.", flatten_controls(text)))
        .unwrap_or_default();
    let issue = job
        .issue_id
        .as_deref()
        .map(|id| format!(" This job tracks issue {id}."))
        .unwrap_or_default();
    let head = format!(
        "Cadence task {} (job {}, revision {}). You are a Devin cloud session and cannot \
         read host paths or invoke the cadence CLI. For company questions, use wiki/guide \
         access only if this session has it; otherwise report knowledge not consulted and \
         ask for authorized source material, inventing no policy. Spec text follows. ",
        task.id, job.id, revision
    );
    let bridge = format!(".{scope}{issue}");
    // CAD-160: the acceptance clause and the report contract are never
    // cut. Criteria that cannot fit whole beside the fixed head refuse
    // the dispatch; everything else (the inlined spec first) gives way.
    let kept = acceptance.len() + SHA_TRAILER.len();
    if head.len() + kept > ENQUEUE_BYTES {
        return Err(criteria_too_long(
            &task.id,
            "kickoff",
            head.len() + kept,
            acceptance.len(),
            ENQUEUE_BYTES,
            spec,
        ));
    }
    let budget = ENQUEUE_BYTES - kept;
    let fixed = head.len() + bridge.len();
    let spec_text = if fixed + cleaned.len() <= budget {
        cleaned
    } else {
        let room = budget.saturating_sub(fixed + SPEC_NOTE.len());
        format!("{}{SPEC_NOTE}", take_bytes(&cleaned, room))
    };
    let mut body = format!("{head}{spec_text}{bridge}{acceptance}{SHA_TRAILER}");
    // Scope and issue text can already exceed the enqueue limit. Drop
    // the spec, say so, and keep the criteria and SHA trailer inside
    // 48_000 bytes.
    if body.len() > ENQUEUE_BYTES {
        const OMITTED: &str = "… (spec text omitted; the prompt was cut to fit)";
        let tail = format!("{OMITTED}{acceptance}{SHA_TRAILER}");
        let room = ENQUEUE_BYTES.saturating_sub(tail.len());
        let prefix = take_bytes(&format!("{head}{bridge}"), room);
        body = format!("{prefix}{tail}");
    }
    Ok(body)
}

/// CAD-160: the refusal when a `kind` ("message" or "kickoff") cannot
/// carry its acceptance criteria whole within `ceiling` — names the
/// ceiling and the spec file, and says nothing was queued. A kickoff
/// has no sender text, so its hint names only the criteria.
pub(super) fn criteria_too_long(
    task: &str,
    kind: &str,
    total: usize,
    criteria: usize,
    ceiling: usize,
    spec: &str,
) -> Error {
    let shorten = if kind == "message" {
        "shorten your text or the criteria"
    } else {
        "shorten the criteria"
    };
    Error::rejected(format!(
        "Task '{task}': the {kind} is {total} bytes with its acceptance criteria \
         ({criteria} bytes) whole — over the {ceiling}-char delivery ceiling. Criteria are \
         never truncated (CAD-160): {shorten}, and keep the detail in the spec file {}. \
         Nothing was queued.",
        flatten_controls(spec)
    ))
}

/// A Devin cloud session: it cannot read host paths, so its kickoff
/// inlines the spec text ([`cloud_kickoff_body`]).
fn cloud_session(provider: &str, endpoint_kind: &str) -> bool {
    provider == "devin" && endpoint_kind == "cloud"
}

/// CAD-160: the size a `job dispatch` kickoff must fit for an assignee
/// on `provider`/`endpoint_kind` — the enqueue limit for a Devin cloud
/// session (its kickoff inlines the spec), else the pty ceiling.
pub fn kickoff_ceiling(provider: &str, endpoint_kind: &str) -> usize {
    if cloud_session(provider, endpoint_kind) {
        ENQUEUE_BYTES
    } else {
        pty::MAX_BODY
    }
}

pub(super) fn kickoff_correlation(message_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(message_id.as_bytes()));
    format!(" Correlation: {}.", &digest[..32])
}

/// The dispatch body — one line, control-char free, ≤4000 chars (the
/// pty constraint that already shapes bootstrap messages). Pointer-first:
/// the spec path, scope claim and acceptance reference, then the exact
/// report contract. Managed endpoints never run `message result`, so
/// they get the `SHA:`-trailer convention instead of `--sha`.
pub(super) fn kickoff_body(
    job: &Job,
    task: &Task,
    revision: i64,
    message_id: &str,
    assignee: &Agent,
) -> Result<String> {
    job_kickoff(
        job,
        task,
        revision,
        message_id,
        &assignee.provider,
        &assignee.endpoint_kind,
    )
}

/// CAD-1373: one short discovery pointer shared by the full and compact
/// job envelopes, so the compact form keeps it. It names the document's
/// installed path and section — `cadence skill status` prints the path
/// for the session's own `$HOME`, so the pointer needs no install.
const KNOWLEDGE_POINTER: &str = " Company questions: read the \"Business knowledge\" section of \
    $HOME/.agents/skills/cadence/SKILL.md.";

/// The `job dispatch` kickoff for an assignee on `provider`/
/// `endpoint_kind`. Public so `cadence dispatch --job` can build the
/// same text before `issue start` and refuse a list that will not fit
/// (CAD-160) instead of leaving a worktree and job behind.
pub fn job_kickoff(
    job: &Job,
    task: &Task,
    revision: i64,
    message_id: &str,
    provider: &str,
    endpoint_kind: &str,
) -> Result<String> {
    if cloud_session(provider, endpoint_kind) {
        return cloud_kickoff_body(job, task, revision);
    }
    let spec = task.spec_path.as_deref().unwrap_or(&job.spec_path);
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect()
    };
    let mut scope = String::new();
    if let Some(w) = &task.worktree {
        scope += &format!(" worktree {},", clean(w));
    }
    if let Some(b) = &task.branch {
        scope += &format!(" branch {},", clean(b));
    }
    if let Some(s) = &task.base_sha {
        scope += &format!(" base {},", clean(s));
    }
    if !scope.is_empty() {
        scope.pop(); // trailing comma
        scope = format!(" Scope:{}.", scope);
    }
    let acceptance = task
        .acceptance
        .as_deref()
        .map(|a| format!(" Acceptance: {}.", clean(a)))
        .unwrap_or_default();
    let issue = job
        .issue_id
        .as_deref()
        .map(|i| {
            format!(
                " This job tracks issue {i} — if you write an agent-note, \
                 put the header line `Issue: {i}` in it."
            )
        })
        .unwrap_or_default();
    let issue_short = job
        .issue_id
        .as_deref()
        .map(|i| format!(" Issue: {i}."))
        .unwrap_or_default();
    let managed = registry::reports_turn_result(provider, endpoint_kind);
    let report = if managed {
        " Report when done: end your final answer with a one-line \
         summary followed by a last line `SHA: <40-hex>` naming the \
         commit you produced — the daemon reads that line as the \
         reported revision."
            .to_string()
    } else {
        format!(
            " Report when done: `cadence message result {message_id} \
             --token <turn_id> --text '<summary>' --sha \"$(git rev-parse \
             HEAD)\"` — `cadence self` shows the turn_id."
        )
    };
    let head = format!(
        "Cadence task {} (job {}, revision {}): implement per spec at {}.",
        task.id,
        job.id,
        revision,
        clean(spec)
    );
    // Explicit envelopes end with the per-message correlation: the
    // screen probe slices the body's tail, and without it every
    // kickoff shares the same boilerplate ending. Managed envelopes
    // take no screen probe and stay byte-identical to before.
    let correlation = if managed {
        String::new()
    } else {
        kickoff_correlation(message_id)
    };
    // CAD-1373: the discovery pointer sits before the report contract
    // in both envelopes, so the managed full form still ends on the
    // reminder, the managed compact form on the report contract, and
    // the explicit form on the correlation tail.
    let discovery = KNOWLEDGE_POINTER;
    let full = format!(
        "{head}{scope}{acceptance}{discovery}{issue}{report} Do not report a SHA you have not \
         committed.{correlation}"
    );
    if full.len() <= pty::MAX_BODY {
        return Ok(full);
    }
    // CAD-160: over the pty ceiling, prose gives way — the issue note
    // shrinks to its id and the closing reminder goes — while the
    // pointers, every acceptance criterion, the report contract and
    // the correlation stay whole. If that still cannot fit, refuse.
    let compact = format!("{head}{scope}{acceptance}{discovery}{issue_short}{report}{correlation}");
    if compact.len() <= pty::MAX_BODY {
        return Ok(compact);
    }
    Err(criteria_too_long(
        &task.id,
        "kickoff",
        compact.len(),
        acceptance.len(),
        pty::MAX_BODY,
        spec,
    ))
}

#[cfg(test)]
mod cad1373_kickoff_pointer_tests {
    use super::*;

    fn job(spec: &str) -> Job {
        Job {
            id: "j1".into(),
            title: None,
            spec_path: spec.into(),
            spec_sha256: None,
            pm_alias: "pm".into(),
            issue_id: None,
            repo: None,
            base_ref: None,
            state: "open".into(),
            max_revisions: 2,
            stall_secs: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        }
    }

    fn task(spec: &str, acceptance: &str) -> Task {
        Task {
            id: "t1".into(),
            job_id: "j1".into(),
            title: None,
            role: "worker".into(),
            assignee: Some("cloud-1".into()),
            spec_path: Some(spec.into()),
            acceptance: Some(acceptance.into()),
            worktree: None,
            branch: None,
            base_sha: None,
            state: "draft".into(),
            head_sha: None,
            revision: 0,
            dispatch_message: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        }
    }

    /// CAD-1373: the cloud kickoff carries the business-knowledge
    /// pointer, keeps its acceptance and SHA trailer, and stays inside
    /// the enqueue budget.
    #[test]
    fn cloud_kickoff_points_company_questions_and_keeps_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("spec.md");
        std::fs::write(&spec, "Do the cloud task.").unwrap();
        let spec = spec.display().to_string();
        let body = cloud_kickoff_body(&job(&spec), &task(&spec, "the task is done"), 1).unwrap();
        assert!(body.len() <= ENQUEUE_BYTES, "{} bytes", body.len());
        assert!(
            body.contains("use wiki/guide access only if this session has it"),
            "{body}"
        );
        assert!(body.contains("report knowledge not consulted"), "{body}");
        assert!(body.contains(" Acceptance: the task is done."), "{body}");
        assert!(body.contains("`SHA: <40-hex>`"), "{body}");
    }

    /// CAD-1373: an acceptance sized so the full envelope crosses the
    /// pty ceiling while the compact one fits; the compact result must
    /// keep the discovery pointer, the whole criteria and the
    /// correlation, and must carry the short issue id, not the long
    /// issue note or the closing reminder the compact form drops.
    #[test]
    fn compact_job_kickoff_keeps_the_knowledge_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("spec.md");
        std::fs::write(&spec, "Do the pty task.").unwrap();
        let spec = spec.display().to_string();
        let mut j = job(&spec);
        j.issue_id = Some("CAD-9".into());
        let worker = Agent {
            uid: "agt-worker".into(),
            alias: "pty-1".into(),
            provider: "pi".into(),
            endpoint_kind: "pty".into(),
            role: "worker".into(),
            team_role: None,
            cwd: dir.path().display().to_string(),
            sandbox: "read-only".into(),
            instructions: None,
            thread_id: None,
            session_id: None,
            model: None,
            effort: None,
            pid: None,
            pid_start: None,
            endpoint: None,
            params: None,
            model_selection: None,
            quota: None,
            generation: None,
            state: "starting".into(),
            enabled: true,
            gc_keep: false,
            archived_at: None,
            archived_by: None,
            archive_reason: None,
            lifecycle_rev: 0,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        // Size the criterion deterministically: measure the envelope
        // with a one-char criterion (it returns the full form), then
        // grow it so the full form crosses the ceiling while the
        // compact form — shorter by the long issue note and the
        // closing reminder it drops — still fits.
        let full0 = kickoff_body(&j, &task(&spec, "x"), 1, "mid-cad1373", &worker).unwrap();
        assert!(full0.len() <= pty::MAX_BODY);
        let long_note = " This job tracks issue CAD-9 — if you write an agent-note, \
                         put the header line `Issue: CAD-9` in it.";
        let reminder = " Do not report a SHA you have not committed.";
        assert!(full0.contains(long_note), "{full0}");
        assert!(full0.contains(reminder), "{full0}");
        let compact0 = full0.len() - long_note.len() + " Issue: CAD-9.".len() - reminder.len();
        let criteria = "c".repeat(pty::MAX_BODY - compact0);
        let t = task(&spec, &criteria);
        let body = kickoff_body(&j, &t, 1, "mid-cad1373", &worker).unwrap();
        // The compact form is provably taken: the full-only prose is
        // gone, the short issue id survives.
        assert!(!body.contains(long_note), "{body}");
        assert!(!body.contains(reminder), "{body}");
        assert!(body.contains(" Issue: CAD-9."), "{body}");
        assert!(body.len() <= pty::MAX_BODY, "{} bytes", body.len());
        assert!(body.contains(" Acceptance: "), "{body}");
        assert!(body.contains(criteria.as_str()), "{body}");
        assert!(body.contains("Business knowledge"), "{body}");
        assert!(body.contains(" Correlation: "), "{body}");
    }
}
