//! `cadence cloud-sandbox` (CAD-1673): run one issue in an AgenticOS cloud
//! sandbox through [`cadence_agent::adapter::sandbox`]. Hosted only: outside an
//! AgenticOS company context nothing is sent. The default host dispatch
//! path is unchanged.
use std::path::Path;
use std::time::Duration;

use cadence_agent::adapter::sandbox::{
    create_remote_lane, drive_job, is_settled, CreateJob, HttpSandboxApi, RemoteLane, ResultRelay,
    RunOutcome, SandboxClient, SandboxEntitlement, SandboxResult, UreqSandboxTransport,
};
use cadence_agent::error::{Error, Result, Structured};
use cadence_agent::issue::{board, model::check_ref_value, task_report, write, Pm};

/// Refusal code outside a hosted AgenticOS company context.
pub const NOT_HOSTED: &str = "cloud_sandbox_not_hosted";
const COMPANY_ENV: &str = "AGENTICOS_COMPANY";
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Polls before an unsettled job is stopped (about an hour).
const MAX_POLLS: u32 = 720;

/// The company this process is bound to. Absent or malformed means the
/// process is not hosted, so the caller refuses before building a request.
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
            "cloud-sandbox needs a hosted AgenticOS context ({COMPANY_ENV}); nothing was sent"
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

/// Side effects of a run, injected so the flow is testable without a
/// tracker.
pub struct Sinks<'a> {
    /// Called once the remote lane exists, before the job is driven.
    pub record: &'a mut dyn FnMut(&RemoteLane) -> Result<()>,
    /// Called once with the relayed result of a completed job.
    pub report: &'a mut dyn FnMut(&RemoteLane, &SandboxResult) -> Result<()>,
}

/// Create the remote lane, record it, poll until the job settles, then
/// drive it. A completed job is relayed and reported once. A failed job is
/// stopped by the seam and reports nothing. An unsettled job past
/// `max_polls` is stopped and errors.
pub fn drive_remote(
    client: &dyn SandboxClient,
    entitlement: &dyn SandboxEntitlement,
    company: &str,
    req: &CreateJob,
    interval: Duration,
    max_polls: u32,
    sinks: Sinks<'_>,
) -> Result<RunOutcome> {
    let (lane, job) = create_remote_lane(client, entitlement, company, req)?;
    (sinks.record)(&lane)?;
    let mut polls = 0;
    loop {
        if is_settled(&client.get_job(&job.job_id)?.state) {
            break;
        }
        polls += 1;
        if polls >= max_polls {
            client.stop_job(&job.job_id)?;
            return Err(Error::rejected(format!(
                "cloud sandbox job {} did not settle; it was stopped",
                job.job_id
            )));
        }
        std::thread::sleep(interval);
    }
    let relay = ResultRelay::default();
    let outcome = drive_job(client, &relay, &job, false)?;
    if let RunOutcome::Relayed(result) = &outcome {
        (sinks.report)(&lane, result)?;
    }
    Ok(outcome)
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
        evidence = result.text.replace("\n## ", "\n### "),
    );
    let prepared = task_report::prepare(pm, &text, Some(issue), Some(task_report::Kind::Done))?;
    task_report::store(pm, &prepared, actor)?;
    Ok(())
}

/// `cadence cloud-sandbox <ISSUE> --branch <b> --base-sha <sha> [--spec f]`.
pub fn run(issue: &str, branch: &str, base_sha: &str, spec: Option<&Path>) -> Result<i32> {
    let company = hosted_company(|k| std::env::var(k).ok())?;
    if branch.trim().is_empty() {
        return Err(Error::usage("--branch must not be empty"));
    }
    check_ref_value(branch)?;
    if !matches!(base_sha.len(), 40 | 64) || !base_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::usage("--base-sha must be a full commit sha"));
    }
    let pm = Pm::open_default()?;
    let issue_record = board::find_issue(&pm.dir, issue)?;
    let spec = match spec {
        Some(path) => std::fs::read_to_string(path)?,
        None => issue_record.body.clone(),
    };
    let base_sha = base_sha.to_ascii_lowercase();
    let req = CreateJob {
        ticket_id: issue.to_string(),
        branch: branch.to_string(),
        idempotency_key: format!("cloud-sandbox:{issue}:{branch}:{base_sha}"),
        base_sha,
        spec,
    };
    let client = HttpSandboxApi::new(UreqSandboxTransport::new(None));
    let actor = task_report::default_agent();
    let mut record = |lane: &RemoteLane| -> Result<()> {
        write::add_ref(
            &pm,
            issue,
            "branch",
            &lane.branch,
            Some("cloud sandbox"),
            None,
            None,
            None,
            &actor,
        )?;
        write::add_ref(
            &pm,
            issue,
            "note",
            &format!("cloud-sandbox sandbox={} job={}", lane.sandbox_id, lane.job_id),
            Some("cloud sandbox lane"),
            None,
            None,
            None,
            &actor,
        )?;
        Ok(())
    };
    let mut report = |lane: &RemoteLane, result: &SandboxResult| -> Result<()> {
        file_done_report(&pm, issue, lane, result, &actor)
    };
    let outcome = drive_remote(
        &client,
        &CreateEnforcesEntitlement,
        &company,
        &req,
        POLL_INTERVAL,
        MAX_POLLS,
        Sinks {
            record: &mut record,
            report: &mut report,
        },
    )?;
    match outcome {
        RunOutcome::Relayed(_) => {
            println!("cloud sandbox run for {issue} completed and was reported once");
            Ok(0)
        }
        RunOutcome::Failed => Err(Error::rejected(
            "cloud sandbox job failed; it was stopped and nothing was reported",
        )),
        RunOutcome::Stopped => Err(Error::rejected(
            "cloud sandbox job stopped before a result; nothing was reported",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadence_agent::adapter::sandbox::{
        JobSnapshot, SandboxJob, SandboxResult, ENTITLEMENT_REFUSED,
    };
    use std::cell::RefCell;
    use std::sync::Mutex;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Scripted client: queued snapshots for `get_job`, a call log, and an
    /// optional create refusal. No network.
    struct Fake {
        create: Mutex<Option<Result<SandboxJob>>>,
        snapshots: Mutex<Vec<JobSnapshot>>,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(snapshots: Vec<JobSnapshot>) -> Self {
            Self {
                create: Mutex::new(None),
                snapshots: Mutex::new(snapshots.into_iter().rev().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn refusing(code: &str) -> Self {
            let fake = Self::new(vec![]);
            *fake.create.lock().unwrap() = Some(Err(Error::Structured(Structured {
                kind: "rejected",
                code: code.to_string(),
                message: "refused".into(),
                revision: None,
            })));
            fake
        }
    }

    impl SandboxClient for Fake {
        fn create_job(&self, _: &CreateJob) -> Result<SandboxJob> {
            self.calls.lock().unwrap().push("create".into());
            if let Some(err) = self.create.lock().unwrap().take() {
                return err;
            }
            Ok(SandboxJob {
                job_id: "job-1".into(),
                sandbox_id: "csb_job-1".into(),
                branch: "cad-1673".into(),
                expires_at: "x".into(),
                gateway_token: "t".into(),
            })
        }
        fn get_job(&self, _: &str) -> Result<JobSnapshot> {
            self.calls.lock().unwrap().push("get".into());
            let mut queue = self.snapshots.lock().unwrap();
            // Repeat the final snapshot once the script runs out.
            if queue.len() > 1 {
                Ok(queue.pop().unwrap())
            } else {
                Ok(queue.last().cloned().unwrap())
            }
        }
        fn stop_job(&self, _: &str) -> Result<String> {
            self.calls.lock().unwrap().push("stop".into());
            Ok("stopped".into())
        }
    }

    struct Entitled(bool);
    impl SandboxEntitlement for Entitled {
        fn cloud_sandbox_enabled(&self, _: &str) -> Result<bool> {
            Ok(self.0)
        }
    }

    fn snap(state: &str) -> JobSnapshot {
        JobSnapshot {
            job_id: "job-1".into(),
            state: state.into(),
            events: vec![],
            result: SandboxResult {
                sha: Some(SHA.into()),
                diff: None,
                pr: None,
                text: "done".into(),
            },
        }
    }

    fn req() -> CreateJob {
        CreateJob {
            ticket_id: "CAD-1673".into(),
            branch: "cad-1673".into(),
            base_sha: SHA.into(),
            spec: "spec".into(),
            idempotency_key: "k".into(),
        }
    }

    fn no_lane(_: &RemoteLane) -> Result<()> {
        panic!("no lane must be recorded")
    }
    fn no_report(_: &RemoteLane, _: &SandboxResult) -> Result<()> {
        panic!("nothing must be reported")
    }

    fn run_with(
        client: &Fake,
        ent: &dyn SandboxEntitlement,
        record: &mut dyn FnMut(&RemoteLane) -> Result<()>,
        report: &mut dyn FnMut(&RemoteLane, &SandboxResult) -> Result<()>,
    ) -> Result<RunOutcome> {
        drive_remote(
            client,
            ent,
            "co-1",
            &req(),
            Duration::ZERO,
            5,
            Sinks { record, report },
        )
    }

    #[test]
    fn hosted_path_creates_lane_records_refs_and_reports_once() {
        let client = Fake::new(vec![snap("running"), snap("completed")]);
        let recorded = RefCell::new(Vec::new());
        let reported = RefCell::new(Vec::new());
        let out = run_with(
            &client,
            &Entitled(true),
            &mut |l| {
                recorded.borrow_mut().push(l.clone());
                Ok(())
            },
            &mut |_, r| {
                reported.borrow_mut().push(r.text.clone());
                Ok(())
            },
        )
        .unwrap();
        assert!(matches!(out, RunOutcome::Relayed(_)));
        assert_eq!(recorded.borrow().len(), 1);
        assert_eq!(recorded.borrow()[0].sandbox_id, "csb_job-1");
        assert_eq!(recorded.borrow()[0].job_id, "job-1");
        assert_eq!(*reported.borrow(), vec!["done".to_string()]);
        assert_eq!(client.calls.lock().unwrap()[0], "create");
    }

    #[test]
    fn non_hosted_context_refuses_without_company() {
        for env in [None, Some(String::new()), Some("bad company!".to_string())] {
            let err = hosted_company(|_| env.clone()).unwrap_err();
            assert_eq!(err.code(), Some(NOT_HOSTED));
        }
        assert_eq!(hosted_company(|_| Some("co-1".into())).unwrap(), "co-1");
    }

    #[test]
    fn disabled_entitlement_refuses_before_any_wire_call() {
        let client = Fake::new(vec![]);
        let err = run_with(&client, &Entitled(false), &mut no_lane, &mut no_report).unwrap_err();
        assert_eq!(err.code(), Some(ENTITLEMENT_REFUSED));
        assert!(client.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn create_403_not_entitled_refuses_and_records_nothing() {
        let client = Fake::refusing(ENTITLEMENT_REFUSED);
        let err = run_with(
            &client,
            &CreateEnforcesEntitlement,
            &mut no_lane,
            &mut no_report,
        )
        .unwrap_err();
        assert_eq!(err.code(), Some(ENTITLEMENT_REFUSED));
        assert_eq!(*client.calls.lock().unwrap(), vec!["create".to_string()]);
    }

    #[test]
    fn failed_job_is_stopped_and_reports_nothing() {
        let client = Fake::new(vec![snap("failed")]);
        let out = run_with(
            &client,
            &CreateEnforcesEntitlement,
            &mut |_| Ok(()),
            &mut no_report,
        )
        .unwrap();
        assert!(matches!(out, RunOutcome::Failed));
        assert_eq!(client.calls.lock().unwrap().last().unwrap(), "stop");
    }
}
