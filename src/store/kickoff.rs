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
pub(super) fn last_sha_line(text: &str) -> Option<String> {
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
         read host paths or invoke the cadence CLI. Spec text follows. ",
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
    let full = format!(
        "{head}{scope}{acceptance}{issue}{report} Do not report a SHA you have not \
         committed.{correlation}"
    );
    if full.len() <= pty::MAX_BODY {
        return Ok(full);
    }
    // CAD-160: over the pty ceiling, prose gives way — the issue note
    // shrinks to its id and the closing reminder goes — while the
    // pointers, every acceptance criterion, the report contract and
    // the correlation stay whole. If that still cannot fit, refuse.
    let compact = format!("{head}{scope}{acceptance}{issue_short}{report}{correlation}");
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
