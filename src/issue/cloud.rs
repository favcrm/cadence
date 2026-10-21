//! Cloud sandbox dispatch backend (CAD-1673).
//!
//! A registered `sandbox` agent is dispatched through the normal
//! `dispatch_task` kickoff. The daemon consumes that kickoff here: create the
//! AgenticOS job, poll it, complete the kickoff, and file the normal done
//! report. There is no separate CLI command and the remote runner does not
//! depend on the host worktree.

use std::time::Duration;

use serde_json::{json, Value};

use crate::adapter::sandbox::{
    drive_job, is_settled, normalize_github_repo, CreateJob, HttpSandboxApi, RemoteLane,
    ResultRelay, RunOutcome, SandboxClient, SandboxJob, SandboxResult, UreqSandboxTransport,
    SANDBOX_SPEC_BYTES,
};
use crate::error::{Error, Result, Structured};
use crate::issue::{task_report, write, Pm};
use crate::store::{Agent, Job, Message, Store, Task};

/// Refusal code outside a hosted AgenticOS company context.
pub const NOT_HOSTED: &str = "cloud_sandbox_not_hosted";
/// A poll failed and the remote job could not be proven stopped. The
/// kickoff is left running, so a retry re-enters the same remote job
/// rather than starting a second writer.
pub const UNCERTAIN: &str = "cloud_sandbox_uncertain";
/// An operator stop reached the poller; the remote job was stopped and the
/// kickoff is settled as interrupted, never as a completed report.
pub const STOPPED: &str = "cloud_sandbox_stopped";
const COMPANY_ENV: &str = "AGENTICOS_COMPANY";
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Consecutive transport errors tolerated while polling before the job is
/// stopped and the delivery is settled.
const POLL_ERROR_RETRIES: u32 = 12;
/// Polls before an unsettled job is stopped (about an hour).
const MAX_POLLS: u32 = 720;

/// The company this process is bound to. Absent or malformed means the
/// process is not hosted, so nothing is sent.
pub fn hosted_company(read: impl Fn(&str) -> Option<String>) -> Result<String> {
    let company = read(COMPANY_ENV).unwrap_or_default();
    let ok = (1..=64).contains(&company.len())
        && company
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        return Ok(company);
    }
    Err(Error::Structured(Structured {
        kind: "rejected",
        code: NOT_HOSTED.to_string(),
        message: format!(
            "cloud sandbox needs a hosted AgenticOS context ({COMPANY_ENV}); nothing was sent"
        ),
        revision: None,
    }))
}

/// The AOS-324 create request for a dispatch body. The pre-dispatch
/// admission and the consumer share this builder, so both name the same
/// idempotency key, branch, base and spec. The key is the durable kickoff
/// message id: retries of the same kickoff (including after an uncertain
/// poll) re-enter the original remote job, while a re-dispatch after a
/// terminal failure mints a new kickoff and a new job.
fn create_request(job: &Job, task: &Task, message_id: &str, body: &str) -> Result<CreateJob> {
    let issue = job.issue_id.as_deref().ok_or_else(|| {
        Error::rejected("cloud sandbox dispatch needs a job bound to a tracker issue")
    })?;
    let root = job.repo.as_deref().ok_or_else(|| {
        Error::rejected("cloud sandbox dispatch needs the job's repository checkout")
    })?;
    let repo = repo_remote(root)?;
    let branch = task
        .branch
        .as_deref()
        .ok_or_else(|| Error::rejected("cloud sandbox dispatch needs the task branch"))?;
    let base_sha = task
        .base_sha
        .as_deref()
        .ok_or_else(|| Error::rejected("cloud sandbox dispatch needs the task base commit"))?;
    let spec = match task.worktree.as_deref() {
        Some(path) if !path.is_empty() => body.replace(path, "."),
        _ => body.to_string(),
    };
    if spec.len() > SANDBOX_SPEC_BYTES {
        return Err(Error::rejected(format!(
            "cloud sandbox kickoff is {} bytes; the AgenticOS spec limit is {SANDBOX_SPEC_BYTES}",
            spec.len()
        )));
    }
    Ok(CreateJob {
        ticket_id: issue.to_string(),
        repo,
        branch: branch.to_string(),
        base_sha: base_sha.to_string(),
        spec,
        idempotency_key: format!("cloud-sandbox:{}:{}", task.id, message_id),
    })
}

/// Authoritative admission before any durable dispatch write. AOS enforces
/// the company's `cloud_sandbox` entitlement on create (403
/// `cloud_sandbox_not_entitled` maps to `ENTITLEMENT_REFUSED`), and the
/// consumer later replays the same idempotency key. A refusal therefore
/// leaves tracker, lane, task and message untouched.
pub fn admit_dispatch(job: &Job, task: &Task, message_id: &str, body: &str) -> Result<SandboxJob> {
    let _ = hosted_company(|key| std::env::var(key).ok())?;
    let request = create_request(job, task, message_id, body)?;
    let client = HttpSandboxApi::new(UreqSandboxTransport::new(None));
    client.create_job(&request)
}

/// Stop an admission whose local dispatch did not commit. A stop failure
/// is deliberately not fatal to the caller: the remote may still run, and
/// the task/message were never written, so there is no consumer to
/// duplicate it.
pub fn stop_admission(job: &SandboxJob) {
    let client = HttpSandboxApi::new(UreqSandboxTransport::new(None));
    let _ = client.stop_job(&job.job_id);
}

/// Consume one dispatched kickoff for a sandbox agent.
///
/// The caller has already run the normal dispatch transaction, so the task is
/// `dispatched` and `message` is the kickoff. This function marks it running,
/// creates and drives the remote job, then finishes the same message. The
/// normal finish path moves the task to review and routes the PM notice; the
/// remote job is the worker.
pub fn run_dispatch(
    pm: &Pm,
    store: &Store,
    task: &Task,
    job: &Job,
    message: &Message,
    agent: &Agent,
    stop: &dyn Fn() -> bool,
) -> Result<()> {
    let issue = job.issue_id.as_deref().ok_or_else(|| {
        let error = Error::rejected("cloud sandbox dispatch needs a job bound to a tracker issue");
        fail_message(store, message, &error);
        error
    })?;
    let (lane, result) = match execute(pm, task, job, message, agent, stop) {
        Ok(value) => value,
        Err(error) if error.code() == Some(UNCERTAIN) => {
            // The remote job may still be a writer. Leave the kickoff
            // running; a retry re-enters it by task id + revision.
            return Err(error);
        }
        Err(error) if error.code() == Some(STOPPED) => {
            interrupt_message(store, message, &error);
            return Err(error);
        }
        Err(error) => {
            fail_message(store, message, &error);
            return Err(error);
        }
    };
    let prepared = match prepare_done_report(pm, issue, &lane, &result, &agent.alias) {
        Ok(prepared) => prepared,
        Err(error) => {
            fail_message(store, message, &error);
            return Err(error);
        }
    };
    if let Err(error) = task_report::store(pm, &prepared, &agent.alias) {
        fail_message(store, message, &error);
        return Err(error);
    }
    store.finish(message, "completed", &result_json(&result), None)?;
    Ok(())
}

pub(crate) fn fail_message(store: &Store, message: &Message, error: &Error) {
    let error_text = error.to_string();
    let result = json!({
        "status": "failed",
        "text": error_text.clone(),
        "error": error_text.clone(),
    });
    let _ = store.finish(message, "failed", &result, Some(error_text.as_str()));
}

/// Settle an operator-stopped kickoff without publishing a done report.
pub fn interrupt_message(store: &Store, message: &Message, error: &Error) {
    let text = format!("cloud sandbox dispatch stopped: {error}");
    let result = json!({
        "status": "interrupted",
        "text": text,
    });
    let _ = store.finish(message, "interrupted", &result, Some(text.as_str()));
}

fn execute(
    pm: &Pm,
    task: &Task,
    job: &Job,
    message: &Message,
    agent: &Agent,
    stop: &dyn Fn() -> bool,
) -> Result<(RemoteLane, SandboxResult)> {
    if stop() {
        return Err(Error::Structured(Structured {
            kind: "rejected",
            code: STOPPED.to_string(),
            message: "cloud sandbox dispatch was stopped before create".to_string(),
            revision: None,
        }));
    }
    let request = create_request(job, task, &message.id, &message.body)?;
    let client = HttpSandboxApi::new(UreqSandboxTransport::new(None));
    let job_handle = client.create_job(&request)?;
    if stop() {
        let _ = client.stop_job(&job_handle.job_id);
        return Err(Error::Structured(Structured {
            kind: "rejected",
            code: STOPPED.to_string(),
            message: "cloud sandbox dispatch was stopped after create".to_string(),
            revision: None,
        }));
    }
    let lane = RemoteLane {
        ticket_id: request.ticket_id.clone(),
        branch: job_handle.branch.clone(),
        sandbox_id: job_handle.sandbox_id.clone(),
        job_id: job_handle.job_id.clone(),
    };
    if let Err(error) = write::add_ref(
        pm,
        &request.ticket_id,
        "note",
        &format!(
            "cloud-sandbox sandbox={} job={}",
            lane.sandbox_id, lane.job_id
        ),
        Some("cloud sandbox lane"),
        None,
        None,
        None,
        &agent.alias,
    ) {
        let _ = client.stop_job(&job_handle.job_id);
        return Err(error);
    }
    let result = drive_to_result(&client, &job_handle, stop)?;
    Ok((lane, result))
}

fn polling_failed(client: &dyn SandboxClient, job: &SandboxJob, error: &Error) -> Error {
    match client.stop_job(&job.job_id) {
        Ok(_) => Error::rejected(format!(
            "cloud sandbox job {} polling failed ({error}); it was stopped",
            job.job_id
        )),
        Err(stop) => Error::Structured(Structured {
            kind: "rejected",
            code: UNCERTAIN.to_string(),
            message: format!(
                "cloud sandbox job {} polling failed ({error}); stop also failed ({stop}) — \
                 the job may still run and this kickoff is left running",
                job.job_id
            ),
            revision: None,
        }),
    }
}

fn stopped(client: &dyn SandboxClient, job: &SandboxJob) -> Error {
    let _ = client.stop_job(&job.job_id);
    Error::Structured(Structured {
        kind: "rejected",
        code: STOPPED.to_string(),
        message: format!(
            "cloud sandbox job {} was stopped by the operator",
            job.job_id
        ),
        revision: None,
    })
}

fn drive_to_result(
    client: &dyn SandboxClient,
    job: &SandboxJob,
    stop: &dyn Fn() -> bool,
) -> Result<SandboxResult> {
    let mut polls = 0;
    let mut failures = 0;
    loop {
        if stop() {
            return Err(stopped(client, job));
        }
        let snapshot = match client.get_job(&job.job_id) {
            Ok(snapshot) => {
                failures = 0;
                snapshot
            }
            Err(error) => {
                failures += 1;
                if failures <= POLL_ERROR_RETRIES {
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
                return Err(polling_failed(client, job, &error));
            }
        };
        if stop() {
            return Err(stopped(client, job));
        }
        if is_settled(&snapshot.state) {
            break;
        }
        polls += 1;
        if polls >= MAX_POLLS {
            client.stop_job(&job.job_id)?;
            return Err(Error::rejected(format!(
                "cloud sandbox job {} did not settle; it was stopped",
                job.job_id
            )));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    if stop() {
        return Err(stopped(client, job));
    }
    let relay = ResultRelay::default();
    let outcome = match drive_job(client, &relay, job, false) {
        Ok(outcome) => outcome,
        Err(error) => return Err(polling_failed(client, job, &error)),
    };
    match outcome {
        RunOutcome::Relayed(result) => Ok(result),
        RunOutcome::Failed => Err(Error::rejected(
            "cloud sandbox job failed; it was stopped and nothing was reported",
        )),
        RunOutcome::Stopped => Err(Error::rejected(
            "cloud sandbox job stopped before a result; nothing was reported",
        )),
    }
}

fn result_json(result: &SandboxResult) -> Value {
    let text = redact_sandbox_tokens(&normalize_controls(&result.text));
    let diff = result.diff.as_deref().map(diff_summary);
    json!({
        "sha": result.sha.clone(),
        "diff": diff,
        "pr": result.pr.clone(),
        "text": text,
    })
}

/// The project's GitHub remote from the job's checkout. The job records the
/// host repo root; the remote is normalised to `https://github.com/owner/repo`
/// before anything is sent.
fn repo_remote(root: &str) -> Result<String> {
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["remote", "get-url", "origin"]),
    )
    .map_err(|e| Error::rejected(format!("cloud sandbox repo remote is unreadable: {e}")))?;
    if !out.status.success() {
        return Err(Error::rejected(
            "cloud sandbox repo has no readable origin remote",
        ));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    normalize_github_repo(raw.trim())
}

/// A short, fence-free summary of a unified diff. Raw diff text is never
/// embedded in a report: a context line can contain the report scanner's
/// fence, and a single long line can exceed the report byte cap.
fn diff_summary(diff: &str) -> String {
    let mut files = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            files += 1;
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk {
            if line.starts_with('+') {
                added += 1;
            } else if line.starts_with('-') {
                removed += 1;
            }
        }
    }
    format!("Diff summary: {files} file(s), +{added} -{removed}.")
}

/// Match the report writer's control stripping before scanning for the token.
/// Otherwise `csj\r1.` bypasses the literal prefix and is reconstructed by
/// `task_report::prepare` after the redactor has run.
fn normalize_controls(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// Remove the sandbox gateway token from untrusted runner text. The token is
/// AgenticOS-owned and must never reach a durable report; the report secret
/// guard does not know the `csj1.` format.
fn redact_sandbox_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("csj1.") {
        out.push_str(&rest[..pos]);
        out.push_str("<redacted-sandbox-token>");
        let after = &rest[pos + 5..];
        let end = after
            .find(|c: char| {
                !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '=')
            })
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Build the report evidence: the runner's text plus a bounded diff summary.
fn done_evidence(result: &SandboxResult) -> String {
    let mut evidence =
        redact_sandbox_tokens(&normalize_controls(&result.text)).replace("\n## ", "\n### ");
    if let Some(diff) = &result.diff {
        let diff = diff.trim();
        if !diff.is_empty() {
            evidence.push_str("\n\n### Diff summary\n\n");
            evidence.push_str(&diff_summary(diff));
        }
    }
    evidence
}

/// Prepare the relayed result through the existing report path as a `done`
/// report on the issue, attributed to the cloud agent rather than the daemon
/// process's environment.
fn prepare_done_report(
    pm: &Pm,
    issue: &str,
    lane: &RemoteLane,
    result: &SandboxResult,
    actor: &str,
) -> Result<task_report::Prepared> {
    let mut yaml = format!("task: {issue}\nkind: done\n");
    if let Some(sha) = &result.sha {
        let hex = matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex {
            return Err(Error::rejected("cloud sandbox result sha is not a commit"));
        }
        yaml.push_str(&format!("sha: {sha}\n"));
    }
    if let Some(pr) = &result.pr {
        task_report::parse_pr_url(pr).map_err(|_| {
            Error::rejected("cloud sandbox result PR is not a github.com pull request URL")
        })?;
        yaml.push_str(&format!("pr: {pr}\n"));
    }
    let evidence = done_evidence(result);
    let text = format!(
        "---\n{yaml}---\n\n\
         ## Expected\n\nIssue {issue} runs in cloud sandbox {sandbox} on branch {branch}, job {job}.\n\n\
         ## Evidence\n\n{evidence}\n\n\
         ## Cause\n\nNot applicable: the sandbox job completed.\n\n\
         ## Correction\n\nNone.\n\n\
         ## Lesson\n\nNone.\n\n\
         ## Next\n\nReview the branch against its base before merge.\n",
        sandbox = lane.sandbox_id,
        branch = lane.branch,
        job = lane.job_id,
    );
    task_report::prepare_as(pm, &text, Some(issue), Some(task_report::Kind::Done), actor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_evidence_is_bounded_and_redacts_tokens() {
        let long = "x".repeat(40_000);
        let result = SandboxResult {
            sha: None,
            diff: Some(format!(
                "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-```\n+{long}\n"
            )),
            pr: None,
            text: "token csj\r1.abc.def done".into(),
        };
        let evidence = done_evidence(&result);
        assert!(evidence.contains("### Diff summary"), "{evidence}");
        assert!(evidence.contains("1 file(s), +1 -1"), "{evidence}");
        assert!(!evidence.contains("```"), "{evidence}");
        assert!(!evidence.contains(&long), "raw diff line was embedded");
        assert!(!evidence.contains("csj1."), "{evidence}");
        assert!(!evidence.contains("csj\r1."), "{evidence}");
        assert!(evidence.contains("<redacted-sandbox-token>"), "{evidence}");
        assert!(evidence.len() < 1024, "evidence must stay small");
    }

    #[test]
    fn hosted_company_refuses_unbound_context() {
        assert!(hosted_company(|_| None).is_err());
        assert!(hosted_company(|_| Some("bad/company".into())).is_err());
        assert_eq!(
            hosted_company(|_| Some("co-1".into())).unwrap(),
            "co-1".to_string()
        );
    }

    fn cloud_job(repo: &std::path::Path) -> Job {
        Job {
            id: "j1".into(),
            title: None,
            spec_path: "/s.md".into(),
            spec_sha256: None,
            pm_alias: "pm".into(),
            issue_id: Some("CAD-1".into()),
            repo: Some(repo.display().to_string()),
            base_ref: None,
            state: "open".into(),
            max_revisions: 2,
            stall_secs: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        }
    }

    fn cloud_task(repo: &std::path::Path, revision: i64) -> Task {
        Task {
            id: "t1".into(),
            job_id: "j1".into(),
            title: None,
            role: "worker".into(),
            assignee: Some("sandbox-1".into()),
            spec_path: None,
            acceptance: None,
            worktree: Some(repo.display().to_string()),
            branch: Some("cad-1".into()),
            base_sha: Some("0".repeat(40)),
            head_sha: None,
            state: "draft".into(),
            revision,
            dispatch_message: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        }
    }

    #[test]
    fn create_request_is_bounded_and_message_keyed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "remote",
                "add",
                "origin",
                "https://github.com/favcrm/cadence",
            ],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git: {out:?}");
        }
        let job = cloud_job(&repo);
        let task = cloud_task(&repo, 3);
        let request = create_request(&job, &task, "m1", "body").unwrap();
        assert_eq!(request.spec, "body");
        assert_eq!(request.idempotency_key, "cloud-sandbox:t1:m1");
        let mut next = task.clone();
        next.revision = 4;
        assert_eq!(
            create_request(&job, &next, "m1", "body")
                .unwrap()
                .idempotency_key,
            request.idempotency_key
        );
        assert_ne!(
            create_request(&job, &task, "m2", "body")
                .unwrap()
                .idempotency_key,
            request.idempotency_key
        );
        let long = "x".repeat(SANDBOX_SPEC_BYTES + 1);
        let err = create_request(&job, &task, "m1", &long)
            .unwrap_err()
            .to_string();
        assert!(err.contains(&SANDBOX_SPEC_BYTES.to_string()), "{err}");
    }

    struct StopFails;
    impl SandboxClient for StopFails {
        fn create_job(&self, _request: &CreateJob) -> Result<SandboxJob> {
            Err(Error::internal("unused"))
        }
        fn get_job(&self, _job_id: &str) -> Result<crate::adapter::sandbox::JobSnapshot> {
            Err(Error::internal("unused"))
        }
        fn stop_job(&self, _job_id: &str) -> Result<String> {
            Err(Error::internal("stop failed"))
        }
    }

    struct StopOk;
    impl SandboxClient for StopOk {
        fn create_job(&self, _request: &CreateJob) -> Result<SandboxJob> {
            Err(Error::internal("unused"))
        }
        fn get_job(&self, _job_id: &str) -> Result<crate::adapter::sandbox::JobSnapshot> {
            Err(Error::internal("unused"))
        }
        fn stop_job(&self, _job_id: &str) -> Result<String> {
            Ok("stopped".into())
        }
    }

    #[test]
    fn unproven_stop_is_uncertain_but_a_confirmed_stop_is_terminal() {
        let job = SandboxJob {
            job_id: "job-1".into(),
            sandbox_id: "csb_1".into(),
            branch: "cad-1".into(),
            expires_at: "2026-10-21T05:00:00.000Z".into(),
            gateway_token: "tok".into(),
        };
        let error = polling_failed(&StopFails, &job, &Error::internal("poll failed"));
        assert_eq!(error.code(), Some(UNCERTAIN));
        let stopped = polling_failed(&StopOk, &job, &Error::internal("poll failed"));
        assert!(stopped.code().is_none());
        assert!(stopped.to_string().contains("it was stopped"));
    }
}
