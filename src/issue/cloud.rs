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
    create_remote_lane, drive_job, is_settled, normalize_github_repo, CreateJob, HttpSandboxApi,
    RemoteLane, ResultRelay, RunOutcome, SandboxClient, SandboxEntitlement, SandboxResult,
    UreqSandboxTransport,
};
use crate::error::{Error, Result, Structured};
use crate::issue::{task_report, write, Pm};
use crate::store::{Agent, Job, Message, Store, Task};

/// Refusal code outside a hosted AgenticOS company context.
pub const NOT_HOSTED: &str = "cloud_sandbox_not_hosted";
const COMPANY_ENV: &str = "AGENTICOS_COMPANY";
const POLL_INTERVAL: Duration = Duration::from_secs(5);
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

/// Entitlement is the company's `cloud_sandbox` row, enforced by
/// `api.internal` on job create (403 `cloud_sandbox_not_entitled`, which the
/// seam maps to `ENTITLEMENT_REFUSED`). No separate probe route exists, so
/// the daemon-side gate defers to that refusal.
pub struct CreateEnforcesEntitlement;

impl SandboxEntitlement for CreateEnforcesEntitlement {
    fn cloud_sandbox_enabled(&self, _company: &str) -> Result<bool> {
        Ok(true)
    }
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
    company: &str,
) -> Result<()> {
    let issue = job.issue_id.as_deref().ok_or_else(|| {
        Error::rejected("cloud sandbox dispatch needs a job bound to a tracker issue")
    })?;
    let result = match execute(pm, store, task, job, message, agent, company) {
        Ok((lane, result)) => (lane, result),
        Err(error) => {
            let error_text = error.to_string();
            let result = json!({
                "status": "failed",
                "text": error_text.clone(),
                "error": error_text.clone(),
            });
            let _ = store.finish(message, "failed", &result, Some(error_text.as_str()));
            return Err(error);
        }
    };
    let (lane, result) = result;
    store.finish(message, "completed", &result_json(&result), None)?;
    file_done_report(pm, issue, &lane, &result, &agent.alias)?;
    Ok(())
}

fn execute(
    pm: &Pm,
    store: &Store,
    task: &Task,
    job: &Job,
    message: &Message,
    agent: &Agent,
    company: &str,
) -> Result<(RemoteLane, SandboxResult)> {
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
        Some(path) if !path.is_empty() => message.body.replace(path, "."),
        _ => message.body.clone(),
    };
    let request = CreateJob {
        ticket_id: issue.to_string(),
        repo,
        branch: branch.to_string(),
        base_sha: base_sha.to_string(),
        spec,
        idempotency_key: format!("cloud-sandbox:{}:r{}", task.id, task.revision),
    };
    let client = HttpSandboxApi::new(UreqSandboxTransport::new(None));
    let (lane, job_handle) =
        create_remote_lane(&client, &CreateEnforcesEntitlement, company, &request)?;
    if let Err(error) = store.mark_running(&message.id, &job_handle.job_id) {
        let _ = client.stop_job(&job_handle.job_id);
        return Err(error);
    }
    if let Err(error) = write::add_ref(
        pm,
        issue,
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
    let result = drive_to_result(&client, &job_handle)?;
    Ok((lane, result))
}

fn drive_to_result(
    client: &dyn SandboxClient,
    job: &crate::adapter::sandbox::SandboxJob,
) -> Result<SandboxResult> {
    let mut polls = 0;
    loop {
        if is_settled(&client.get_job(&job.job_id)?.state) {
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
    let relay = ResultRelay::default();
    match drive_job(client, &relay, job, false)? {
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

/// File the relayed result through the existing report path as a `done`
/// report on the issue.
fn file_done_report(
    pm: &Pm,
    issue: &str,
    lane: &RemoteLane,
    result: &SandboxResult,
    actor: &str,
) -> Result<()> {
    let mut yaml = format!("task: {issue}\nkind: done\n");
    if let Some(sha) = &result.sha {
        let hex = matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex {
            return Err(Error::rejected("cloud sandbox result sha is not a commit"));
        }
        yaml.push_str(&format!("sha: {sha}\n"));
    }
    if let Some(pr) = &result.pr {
        task_report::parse_pr_url(pr)?;
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
    let prepared = task_report::prepare(pm, &text, Some(issue), Some(task_report::Kind::Done))?;
    task_report::store(pm, &prepared, actor)?;
    Ok(())
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
}
