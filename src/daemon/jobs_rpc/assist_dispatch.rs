use super::{store, Error, JobIssueSnapshot, Result, Shared};
use crate::daemon::members_rpc::{MemberCaller, MemberReprove};
use crate::issue::{write::issue_dir, Pm, PmLock};
use crate::project_policy::{caller_of, policy_user_actor};
use std::path::PathBuf;

/// CAD-1380: the private issue location and member provenance captured before
/// taking the tracker lock. Admission still needs the locked revalidation and
/// the store's fenced provenance check.
pub(super) struct AssistDispatchAdmission<'plan> {
    pm: Pm,
    issue: &'plan str,
    issue_dir: PathBuf,
    member: MemberCaller,
    pub(super) project: String,
    pub(super) expect: store::AssistDispatchExpect,
}

impl<'plan> AssistDispatchAdmission<'plan> {
    pub(super) fn capture(
        shared: &Shared,
        plan: &'plan store::DispatchPlan,
    ) -> Result<Option<Self>> {
        let Some(assist) = shared.store.assist_request_for_job(&plan.job.id)? else {
            // Member jobs must not fall back to ordinary dispatch if their
            // provenance row is missing. Read errors also propagate above.
            if plan.job.id.starts_with("assist-") {
                return Err(Error::rejected(format!(
                    "task dispatch refused: job '{}' is assist-bound but has no \
                     `assist_requests` provenance — the member's authority cannot be \
                     re-proven at commit (CAD-1380)",
                    plan.job.id
                )));
            }
            return Ok(None);
        };
        let binding = shared
            .store
            .assistant_binding(&assist.org, &assist.sub)?
            .ok_or_else(|| {
                Error::rejected(
                    "task dispatch refused: the assist-bound job has no live \
                     assistant binding for its member (CAD-1380)",
                )
            })?;
        if !binding.is_active() {
            return Err(Error::rejected(format!(
                "task dispatch refused: the assist-bound job's member binding is '{}' — \
                 only an active binding can dispatch (CAD-1380)",
                binding.state
            )));
        }
        let member = MemberCaller {
            org: assist.org.clone(),
            sub: assist.sub.clone(),
            assistant_id: binding.assistant_id.clone(),
            alias: binding.alias.clone(),
            conversation_id: None,
            state: binding.state,
            credential_id: assist.credential_id.clone().unwrap_or_default(),
        };
        let issue = plan.job.issue_id.as_deref().ok_or_else(|| {
            Error::rejected("task dispatch refused: the private job has no issue")
        })?;
        let pm = shared.pm()?;
        let (project, issue_dir) = issue_dir(&pm, issue)?;
        prove_member(
            shared,
            &member,
            &plan.job.id,
            assist.credential_id.as_deref(),
        )?;
        // The planned worker comes from the request or the stored assignee.
        if plan.worker.alias == binding.alias {
            return Err(Error::rejected(format!(
                "task dispatch refused: the assist-bound job cannot be \
                 dispatched to its own assistant '{}' — that is \
                 self-approval (CAD-1380)",
                binding.alias
            )));
        }
        Ok(Some(Self {
            pm,
            issue,
            issue_dir,
            member,
            project: project.key,
            expect: store::AssistDispatchExpect {
                org: assist.org,
                sub: assist.sub,
                assistant_id: binding.assistant_id,
                alias: binding.alias,
                credential_id: assist.credential_id,
                job: plan.job.id.clone(),
            },
        }))
    }

    pub(super) fn lock(&self, snapshot: Option<&JobIssueSnapshot>) -> Result<PmLock> {
        let project_dir = self.issue_dir.parent().ok_or_else(|| {
            Error::rejected("task dispatch refused: issue has no project directory")
        })?;
        // Intake evidence spans the tracker. Non-intake and replay dispatches
        // keep the narrower fence on the private issue and its policy.
        if snapshot.is_some_and(JobIssueSnapshot::has_intake) {
            self.pm.lock()
        } else {
            let policy = project_dir.join("project.yaml");
            self.pm.lock_for_paths(&[self.issue_dir.clone(), policy])
        }
    }

    /// Reprove the issue location and access under the tracker lock, then
    /// check live authority again after the possible wait for that lock.
    pub(super) fn revalidate(&self, shared: &Shared) -> Result<()> {
        let (current, current_dir) = issue_dir(&self.pm, self.issue)?;
        if current_dir != self.issue_dir || current.key != self.project {
            return Err(Error::rejected(
                "task dispatch refused: the issue moved while dispatch was being prepared",
            ));
        }
        let actor = policy_user_actor(&self.member.sub);
        let caller = caller_of(&actor)
            .ok_or_else(|| Error::rejected("task dispatch refused: invalid member identity"))?;
        if !current.allows(caller) {
            return Err(Error::rejected(
                "task dispatch refused: the member can no longer read the issue's project",
            ));
        }
        prove_member(
            shared,
            &self.member,
            &self.expect.job,
            self.expect.credential_id.as_deref(),
        )
    }
}

fn prove_member(
    shared: &Shared,
    member: &MemberCaller,
    job: &str,
    credential_id: Option<&str>,
) -> Result<()> {
    // No member envelope survives to consume time. The enrolled hosted-board
    // service proves the live binding against the queued operation's original
    // credential, never a later mint. Missing service authority fails closed.
    shared.member_authority_live(
        "",
        member,
        MemberReprove {
            operation: "job_dispatch",
            scope: "assist.jobs",
            job_id: Some(job),
            op_credential_id: credential_id,
        },
        "task dispatch",
    )
}
