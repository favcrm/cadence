//! CAD-535: `cadence job` — moved verbatim from src/main.rs.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

mod args;
mod verdict;

use std::path::{Path, PathBuf};

use cadence_agent::adapter::registry;
use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use serde_json::{json, Value};

use super::{print_json, print_line, shape_rows, TaskAction};
pub(crate) use args::JobAction;
use verdict::{verdict_repo, verdict_status_post, verify_worktree};

/// CAD-2215: the forward position of a job-event page — whatever the daemon
/// decided THIS reader may hold. The CLI never chooses the kind: a numeric
/// position exists only on a page whose acknowledgement names a proven
/// operator.
#[derive(Clone, Debug)]
enum JobEventsAfter {
    /// A proven operator's native sequence position.
    Sequence(i64),
    /// Every other reader's opaque token, carried verbatim.
    Token(String),
}

/// The reader a page's `_reader_scope` acknowledgement names. It is proof
/// from the authenticated daemon, never a caller flag or environment
/// value — an alias pane and an operator both run this binary. The ack
/// values live in `board_api`; nothing here restates them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobEventsReader {
    Operator,
    Scoped,
}

/// One protocol refusal: actionable, and never a page.
fn job_events_protocol(detail: &str) -> Error {
    Error::rejected(format!(
        "job events: the daemon answered outside the shipped protocol ({detail}); \
         no page was printed"
    ))
}

/// Take the per-response reader acknowledgement off the page (it is
/// protocol evidence, not payload) and name the reader it proves. A
/// missing or unknown acknowledgement is an actionable protocol error: an
/// older daemon must not be read as a scoped raw numeric feed.
fn job_events_ack(page: &mut Value) -> Result<JobEventsReader> {
    let ack = page
        .as_object_mut()
        .and_then(|body| body.remove(cadence_core::board_api::READER_SCOPE_FIELD));
    match ack.as_ref().and_then(Value::as_str) {
        Some(ack) if ack == cadence_core::board_api::JOB_EVENTS_SCOPE_ACK => {
            Ok(JobEventsReader::Scoped)
        }
        Some(ack) if ack == cadence_core::board_api::JOB_EVENTS_OPERATOR_SCOPE_ACK => {
            Ok(JobEventsReader::Operator)
        }
        _ => Err(Error::rejected(
            "job events: this daemon did not confirm the scoped job-event protocol \
             (_reader_scope is missing or unknown), so no page was printed. Restart \
             the daemon from a build that supports CAD-2215 and retry",
        )),
    }
}

/// Check one page against the reader it acknowledged and return it (the
/// acknowledgement already stripped) for printing. A scoped page must omit
/// global positions entirely, carry a stable keyed `id` and its own forward
/// `cursor` on every visible event, and end on the last visible cursor —
/// the exact supplied token on an empty page, or null on an initial empty
/// page. An operator page keeps its native numeric document.
fn job_events_page(
    page: Value,
    reader: JobEventsReader,
    after: Option<&JobEventsAfter>,
) -> Result<Value> {
    if page.get("has_older").is_some_and(|flag| !flag.is_boolean()) {
        return Err(job_events_protocol("`has_older` is not a boolean"));
    }
    match reader {
        JobEventsReader::Operator => {
            if page.get("events").and_then(Value::as_array).is_none() {
                return Err(job_events_protocol("the page has no `events` array"));
            }
            if page.get("cursor").and_then(Value::as_i64).is_none() {
                return Err(job_events_protocol(
                    "an operator page has no numeric cursor",
                ));
            }
            Ok(page)
        }
        JobEventsReader::Scoped => {
            let events = page
                .get("events")
                .and_then(Value::as_array)
                .ok_or_else(|| job_events_protocol("the page has no `events` array"))?;
            let mut last: Option<&str> = None;
            for event in events {
                let row = event
                    .as_object()
                    .ok_or_else(|| job_events_protocol("an event row is not an object"))?;
                if row.contains_key("seq") || row.contains_key("event_seq") {
                    return Err(job_events_protocol(
                        "a scoped event row carries a global sequence position",
                    ));
                }
                if row
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(job_events_protocol("a scoped event row has no stable id"));
                }
                let cursor = row
                    .get("cursor")
                    .and_then(Value::as_str)
                    .filter(|cursor| !cursor.is_empty())
                    .ok_or_else(|| {
                        job_events_protocol("a scoped event row has no forward cursor")
                    })?;
                last = Some(cursor);
            }
            let echoed = match after {
                Some(JobEventsAfter::Token(token)) => Some(token.as_str()),
                Some(JobEventsAfter::Sequence(_)) | None => None,
            };
            let expected = last.or(echoed);
            let page_cursor = match page.get("cursor") {
                None | Some(Value::Null) => None,
                Some(Value::String(cursor)) if !cursor.is_empty() => Some(cursor.as_str()),
                _ => {
                    return Err(job_events_protocol(
                        "the scoped page cursor is not an opaque token",
                    ))
                }
            };
            if page_cursor != expected {
                return Err(job_events_protocol(
                    "the scoped page cursor is not the last visible event's cursor",
                ));
            }
            Ok(page)
        }
    }
}

/// The position the next request carries: the page's own forward cursor,
/// never a guessed, clamped or defaulted value.
fn job_events_next(page: &Value, reader: JobEventsReader) -> Result<Option<JobEventsAfter>> {
    match reader {
        JobEventsReader::Operator => page
            .get("cursor")
            .and_then(Value::as_i64)
            .map(|position| Some(JobEventsAfter::Sequence(position)))
            .ok_or_else(|| job_events_protocol("an operator page has no numeric cursor")),
        JobEventsReader::Scoped => Ok(page
            .get("cursor")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty())
            .map(|cursor| JobEventsAfter::Token(cursor.to_string()))),
    }
}

/// One `job_events` request in the scoped protocol. `tail` asks for the
/// newest page; otherwise `after` is the forward position (omitted only
/// when there is no position yet).
fn job_events_request(job: &str, after: Option<&JobEventsAfter>, tail: bool, wait: u64) -> Value {
    let mut request = json!({
        "job": job,
        "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
    });
    if tail {
        request["tail"] = json!(true);
    } else {
        match after {
            Some(JobEventsAfter::Sequence(position)) => request["after"] = json!(position),
            Some(JobEventsAfter::Token(token)) => request["after"] = json!(token),
            None => {}
        }
        request["wait"] = json!(wait);
    }
    request
}

/// `--after` as supplied. All-ASCII-digit text is a numeric sequence, which
/// only a proven operator page accepts; anything else is an opaque token
/// carried verbatim. Neither form is ever coerced into the other.
fn job_events_after_argument(raw: &str) -> Result<JobEventsAfter> {
    if raw.is_empty() {
        return Err(Error::rejected(
            "job events: --after needs an opaque cursor or a nonnegative sequence",
        ));
    }
    if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        let position = raw
            .parse::<i64>()
            .map_err(|_| Error::rejected("job events: --after sequence is out of range"))?;
        return Ok(JobEventsAfter::Sequence(position));
    }
    Ok(JobEventsAfter::Token(raw.to_string()))
}

/// CAD-2215: stream one job-event feed through the scoped protocol. Shared
/// by `cadence job events` and `cadence events --job`, so both callers take
/// the same validated page path: numeric positions only for a proven
/// operator page, opaque tokens carried verbatim for every other reader.
/// The daemon proves the reader; the caller never picks the kind.
pub(super) fn stream_job_events(
    state_dir: &Path,
    job: &str,
    after: Option<&str>,
    wait: u64,
    follow: bool,
) -> Result<i32> {
    let rpc = |method: &str, params: Value| client::rpc(state_dir, method, params);
    // The daemon proves which reader this is; these commands only ask for
    // the scoped format and follow the per-page acknowledgement. A numeric
    // --after is a proven operator's position, so the reader is resolved
    // from a tail page before it is ever sent.
    let mut position = after.map(job_events_after_argument).transpose()?;
    let mut reader = None;
    if position.is_none() {
        // Same default as `cadence events`: no --after means the newest
        // page, then forward paging from its cursor.
        let mut page = rpc("job_events", job_events_request(job, None, true, 0))?;
        let ack = job_events_ack(&mut page)?;
        page = job_events_page(page, ack, None)?;
        reader = Some(ack);
        if !cadence_agent::output::stream_json(&page) {
            return Ok(0);
        }
        if !follow {
            return Ok(0);
        }
        position = job_events_next(&page, ack)?;
    } else if matches!(position.as_ref(), Some(JobEventsAfter::Sequence(_))) {
        let mut probe = rpc("job_events", job_events_request(job, None, true, 0))?;
        let ack = job_events_ack(&mut probe)?;
        let _ = job_events_page(probe, ack, None)?;
        if ack != JobEventsReader::Operator {
            return Err(Error::rejected(
                "job events: --after is a numeric sequence, which only a proven \
                 operator page accepts; this reader gets opaque cursors — pass \
                 the `cursor` value a previous page returned instead",
            ));
        }
        reader = Some(ack);
    }
    loop {
        let mut page = rpc(
            "job_events",
            job_events_request(
                job,
                position.as_ref(),
                false,
                if follow { 25 } else { wait },
            ),
        )?;
        let ack = job_events_ack(&mut page)?;
        match reader {
            Some(established) if established != ack => {
                return Err(Error::rejected(
                    "job events: the daemon changed the reader this stream is \
                     scoped for; restart the command",
                ))
            }
            Some(_) => {}
            None => reader = Some(ack),
        }
        if ack == JobEventsReader::Operator
            && matches!(position.as_ref(), Some(JobEventsAfter::Token(_)))
        {
            return Err(Error::rejected(
                "job events: the daemon reports a proven operator, whose cursors \
                 are numeric; the supplied opaque cursor belongs to a scoped reader",
            ));
        }
        let page = job_events_page(page, ack, position.as_ref())?;
        let empty = page
            .get("events")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        if (!empty || !follow) && !cadence_agent::output::stream_json(&page) {
            return Ok(0);
        }
        position = job_events_next(&page, ack)?;
        if !follow {
            return Ok(0);
        }
    }
}

/// The `cadence job` tree — thin RPC wrappers. Validation, transitions
/// and notifications live in the daemon/store so every caller (CLI,
/// agent pane, operator) sees the same rules. `pane`/`by` carry the
/// caller's identity claim: inside a cadence pane `CADENCE_ALIAS` is
/// the actor; outside, `operator`.
pub(super) fn run_job(state_dir: &Path, action: &JobAction) -> Result<i32> {
    let pane = std::env::var("CADENCE_ALIAS").ok();
    // CAD-384: the daemon attributes the caller itself — an agent's pane
    // as that agent, the proven operator as `operator`. Send only what
    // names the caller, never an `operator` default.
    let by = pane.clone();
    let rpc = |method: &str, params: Value| client::rpc(state_dir, method, params);
    match action {
        JobAction::New {
            pm,
            spec,
            job,
            title,
            issue,
            repo,
            base_ref,
            max_revisions,
            stall_secs,
            task_title,
            task_worktree,
            task_branch,
            task_base_sha,
            task_assignee,
        } => {
            // Canonicalize + hash client-side: the daemon stores the
            // path/hash and never needs the board or spec filesystem.
            let spec_path = spec.canonicalize().map_err(|_| {
                Error::rejected(format!("Spec file {} is unreadable", spec.display()))
            })?;
            let bytes = std::fs::read(&spec_path)?;
            use sha2::{Digest, Sha256};
            let spec_sha256 = format!("{:x}", Sha256::digest(&bytes));
            let repo = repo
                .as_ref()
                .map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()));
            print_json(&rpc(
                "job_new",
                json!({"pm": pm, "spec": spec_path, "spec_sha256": spec_sha256,
                       "job": job, "title": title, "issue": issue,
                       "repo": repo, "base_ref": base_ref,
                       "max_revisions": max_revisions,
                       "stall_secs": stall_secs,
                       "task_title": task_title,
                       "task_worktree": task_worktree,
                       "task_branch": task_branch,
                       "task_base_sha": task_base_sha,
                       "task_assignee": task_assignee}),
            )?);
        }
        JobAction::List {
            state,
            all,
            sort,
            limit,
            fields,
            json: _,
        } => {
            let mut out = rpc("job_list", json!({"states": state, "all": all}))?;
            shape_rows(
                &mut out,
                "jobs",
                sort.as_deref(),
                &[
                    ("id", "id"),
                    ("state", "state"),
                    ("title", "title"),
                    ("pm", "pm"),
                    ("issue", "issue"),
                    ("created", "created"),
                    ("updated", "updated"),
                ],
                "id",
                *limit,
                fields,
            )?;
            print_json(&out);
        }
        JobAction::Show { job } => {
            print_json(&rpc("job_show", json!({"job": job}))?);
        }
        JobAction::Events {
            job,
            after,
            wait,
            follow,
        } => return stream_job_events(state_dir, job, after.as_deref(), *wait, *follow),
        JobAction::Dispatch {
            task,
            to,
            ready,
            force,
            message,
            stall_secs,
        } => {
            // --ready is the same operator claim as `send --ready`:
            // resolve the assignee (explicit --to or the stored one),
            // claim the pty gate if there is one, then dispatch. The
            // claim probes the pane — --force overrides a busy verdict.
            if *ready {
                let assignee = match to {
                    Some(a) => Some(a.clone()),
                    None => rpc("task_show", json!({"task": task}))?["task"]["assignee"]
                        .as_str()
                        .map(str::to_string),
                };
                if let Some(assignee) = assignee {
                    let show = rpc("agent_show", json!({"alias": assignee}))?;
                    let agent = &show["agent"];
                    if registry::ready_gate(
                        agent["provider"].as_str().unwrap_or_default(),
                        agent["endpoint_kind"].as_str().unwrap_or_default(),
                    ) {
                        client::rpc_relay(
                            state_dir,
                            "agent_ready",
                            json!({"alias": assignee, "by": pane, "force": force}),
                        )?;
                    }
                }
            }
            let out = rpc(
                "task_dispatch",
                json!({"task": task, "to": to, "message": message,
                       "by": by, "stall_secs": stall_secs}),
            )?;
            // CAD-1353: a duplicate dispatch is an acknowledged retry,
            // not a silent success. Say which message already carries
            // the work and what state it is in, and exit 4 so a script
            // sees the two outcomes differently.
            if let Some(line) = duplicate_dispatch_line(&out) {
                // The line precedes the exit-4 retry verdict: a closed
                // stdout must not panic it into success.
                print_line(&line);
                return Ok(4);
            }
            print_json(&out);
        }
        JobAction::Verdict {
            task,
            sha,
            pass,
            revise,
            blocked,
            reviewer,
            evidence,
            message,
            revision,
            no_verify_worktree,
            no_status,
            pr,
        } => {
            let verdict = if *pass {
                "pass"
            } else if *revise {
                "revise"
            } else if *blocked {
                "blocked"
            } else {
                return Err(Error::rejected(
                    "job verdict needs one of --pass, --revise, --blocked",
                ));
            };
            // `--reviewer` only states who the caller believes it is;
            // the daemon records the verified connection's identity.
            if let Some(claimed) = reviewer {
                let expected = pane.as_deref().unwrap_or("operator");
                if claimed != expected {
                    return Err(Error::rejected(format!(
                        "--reviewer '{claimed}' is not this caller ('{expected}'): the \
                         reviewer is the verified connection (CAD-372) — run the \
                         verdict from the reviewer's own pane, or from an operator \
                         shell for 'operator'"
                    )));
                }
            }
            let evidence = evidence.as_ref().map(std::fs::read_to_string).transpose()?;

            // Worktree verification runs client-side in the job's repo
            // before anything is written; the same task/job fetch feeds
            // the status bridge below. It only gates a verdict that
            // could land — a stale sha or a task not in review stays
            // the store's own rejection.
            let view = rpc("task_show", json!({"task": task}))?["task"].clone();
            let scoped = view["worktree"].is_string() && view["branch"].is_string();
            let landable = view["state"].as_str() == Some("review")
                && view["head_sha"].as_str() == Some(sha.to_ascii_lowercase().as_str());
            let job = if scoped || view["branch"].is_string() {
                rpc("job_show", json!({"job": view["job"]}))?["job"].clone()
            } else {
                Value::Null
            };
            let mut verify = Value::Null;
            if *no_verify_worktree {
                verify = json!({"checked": [], "skipped": [{"check": "worktree verification",
                        "reason": "opted out via --no-verify-worktree"}]});
            } else if scoped && landable {
                let repo = verdict_repo(&job, &view)?;
                verify = verify_worktree(&repo, &view, sha)?;
            }

            let mut out = rpc(
                "task_verdict",
                json!({"task": task, "sha": sha, "verdict": verdict,
                       "evidence": evidence, "message": message,
                       "revision": revision, "verify": verify}),
            )?;

            // The qa-verdict bridge never decides the verdict — every
            // failure reports {posted: false, reason} alongside the
            // committed verdict.
            out["status"] = if *no_status {
                json!({"posted": false, "reason": "skipped via --no-status"})
            } else {
                let job = if job.is_null() {
                    rpc("job_show", json!({"job": view["job"]}))?["job"].clone()
                } else {
                    job
                };
                let revision = out["verdict"]["revision"].as_i64().unwrap_or(0);
                verdict_status_post(&job, &view, sha, verdict, revision, *pr)
            };
            print_json(&out);
        }
        JobAction::Accept { task, merged_sha } => {
            print_json(&rpc(
                "task_accept",
                json!({"task": task, "merged_sha": merged_sha, "by": by}),
            )?);
        }
        JobAction::Cancel { job } => {
            print_json(&rpc("job_cancel", json!({"job": job, "by": by}))?);
        }
        JobAction::Close { job } => {
            print_json(&rpc("job_close", json!({"job": job, "by": by}))?);
        }
        JobAction::Task { action } => match action {
            TaskAction::Add {
                job,
                task,
                title,
                assignee,
                spec,
                accept,
                worktree,
                branch,
                base_sha,
            } => {
                let spec = spec
                    .as_ref()
                    .map(|s| s.canonicalize().unwrap_or_else(|_| s.clone()));
                print_json(&rpc(
                    "task_new",
                    json!({"job": job, "task": task, "title": title,
                           "assignee": assignee, "spec": spec,
                           "acceptance": accept, "worktree": worktree,
                           "branch": branch, "base_sha": base_sha}),
                )?);
            }
            TaskAction::Show { task } => {
                print_json(&rpc("task_show", json!({"task": task}))?);
            }
            TaskAction::Sha { task, sha } => {
                print_json(&rpc(
                    "task_sha",
                    json!({"task": task, "sha": sha, "by": by}),
                )?);
            }
            TaskAction::Fail { task, reason } => {
                print_json(&rpc(
                    "task_fail",
                    json!({"task": task, "reason": reason, "by": by}),
                )?);
            }
            TaskAction::Reopen { task } => {
                print_json(&rpc("task_reopen", json!({"task": task}))?);
            }
            TaskAction::Cancel { task } => {
                print_json(&rpc("task_cancel", json!({"task": task, "by": by}))?);
            }
        },
    }
    Ok(0)
}

/// CAD-1353: the explicit retry line for a duplicate dispatch, or `None`
/// for a fresh one. The caller prints it and exits 4, so a script cannot
/// mistake the retry for a new kickoff.
pub(super) fn duplicate_dispatch_line(out: &Value) -> Option<String> {
    (out.get("duplicate").and_then(Value::as_bool) == Some(true)).then(|| {
        format!(
            "duplicate of {} (state {})",
            out.get("message").and_then(Value::as_str).unwrap_or("?"),
            out.get("message_state")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        )
    })
}

pub(super) fn run(state_dir: PathBuf, action: JobAction) -> Result<i32> {
    run_job(&state_dir, &action)
}
