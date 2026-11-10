//! CAD-534: `cadence daemon` jobs RPC handlers — moved verbatim from src/daemon.rs.

use super::identity::scope_allows_issue;
use super::job_events_opaque;
use super::members_rpc::MemberCaller;
use super::*;
use cadence_core::board_api::{
    JOB_EVENTS_OPERATOR_SCOPE_ACK, JOB_EVENTS_SCOPE_ACK, READER_SCOPE_FIELD,
};
use std::collections::BTreeSet;

/// CAD-2215: the field a relayed, project-scoped `job_list` reply carries to
/// confirm this daemon applied the reader's proof. It is a distinct marker
/// from `board_api::READER_SCOPE_FIELD` — a `job_list` document is not a
/// `master_state` document or a thread page, and must not borrow another
/// verb's confirmation. The board removes it before any client sees it.
pub(crate) const JOB_SCOPE_FIELD: &str = "_job_scope";

/// The exact value of [`JOB_SCOPE_FIELD`]: the daemon narrowed this reply for
/// the relayed reader and re-proved its membership, project scope and
/// private-job denials in the same answer. A missing or different value means
/// the board cannot tell a scoped reply from a global one.
pub(crate) const JOB_SCOPE_ACK: &str = "cad2215.jobs.v1";

/// The settled identity of one member-private job, proven once per scan:
/// a unique request whose owner is the globally settled queue+ledger
/// owner, the job's bound issue, and the project that issue resolves to
/// right now. `None` from the prover means the job is not member-private.
struct PrivateJobContext {
    job: store::Job,
    request: store::AssistRequest,
    issue: String,
    project: String,
}

/// One exact dispatch of a member-private job, proven from the native
/// `task_dispatched` row and the durable kickoff message. The task row is
/// never consulted for revision, assignee, dispatch pointer or `head_sha`,
/// so a reopened and re-dispatched task keeps its historical proof.
struct PrivateDispatchBinding {
    message: store::Message,
    revision: i64,
    assignee: String,
    project: String,
}

/// A binding plus the dispatch's durable completed result: the canonical
/// typed report SHA and the exact stored result body.
struct PrivateDispatchProof {
    binding: PrivateDispatchBinding,
    sha: Option<String>,
    body: String,
}

impl Shared {
    // ---- Jobs: the work axis (docs/JOBS.md) ----

    pub(super) fn may_read_job(&self, caller: &AgentCaller, job: &store::Job) -> Result<bool> {
        if matches!(caller, AgentCaller::Operator) {
            return Ok(true);
        }
        match job.issue_id.as_deref().filter(|issue| !issue.is_empty()) {
            Some(issue) => self.issue_readable_for(caller, issue),
            None => Ok(false),
        }
    }

    fn read_job_for(&self, peer_pid: u32, verb: &str, id: &str) -> Result<store::Job> {
        let caller = self.agent_caller(peer_pid, verb)?;
        let job = self.store.job(id)?;
        if !self.may_read_job(&caller, &job)? {
            return Err(Error::rejected(format!("No such job '{id}'")));
        }
        Ok(job)
    }

    pub(super) fn read_task_for(&self, caller: &AgentCaller, id: &str) -> Result<store::Task> {
        let task = self.store.task(id)?;
        let admitted = match self.store.job(&task.job_id) {
            Ok(job) => self.may_read_job(caller, &job)?,
            Err(Error::Rejected(_)) => matches!(caller, AgentCaller::Operator),
            Err(error) => return Err(error),
        };
        if !admitted {
            return Err(Error::rejected(format!("No such task '{id}'")));
        }
        Ok(task)
    }

    /// `job new` — bookkeeping, not spawning. Requires a registered PM
    /// (an inbox alias is a legitimate PM — notifications drain through
    /// `cadence inbox`) and a readable spec the caller already hashed.
    pub(super) fn rpc_job_new(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_identity_fields(params, "job new")?;
        // CAD-422: a job binds the caller — the operator, or the agent
        // the job names as its PM creating its own job. A peer PM-ing a
        // job to another alias is refused. The caller is proved before
        // the alias resolves, so a refused caller learns nothing about
        // the registry.
        let caller = self.agent_caller(peer_pid, "job new")?;
        let pm = self.resolve_alias(required_str(params, "pm")?)?;
        if let AgentCaller::Agent(alias) = &caller {
            if *alias != pm {
                return Err(Error::rejected(format!(
                    "job new refused: agent '{alias}' may create jobs only under its \
                     own PM alias (this job names '{pm}') — the operator creates \
                     jobs for others (caller rule, CAD-422)"
                )));
            }
            let issue = optional_str(params, "issue")
                .filter(|issue| !issue.is_empty())
                .ok_or_else(|| {
                    Error::rejected("job new refused: an agent needs a readable issue")
                })?;
            if !self.issue_readable_for(&caller, issue)? {
                return Err(Error::rejected(format!(
                    "Unknown issue '{issue}' — `cadence issue ls` lists what exists"
                )));
            }
        }
        let spec = required_str(params, "spec")?;
        let spec_sha256 = required_str(params, "spec_sha256")?;
        // The canonical issue's effective spec is its complete body, not
        // tracker metadata changed by claim/lane/message-ref bookkeeping.
        // This new pin is server-derived; accepted jobs are never refreshed.
        let canonical_digest = match (optional_str(params, "issue"), self.pm()) {
            (Some(issue), Ok(pm)) => crate::issue::dispatch::canonical_issue_spec_digest(
                &pm,
                issue,
                std::path::Path::new(spec),
                true,
            )?,
            _ => None,
        };
        let spec_sha256 = canonical_digest.as_deref().unwrap_or(spec_sha256);
        let id = optional_str(params, "job")
            .map(str::to_string)
            .unwrap_or_else(|| format!("job-{}", &Uuid::new_v4().simple().to_string()[..8]));
        let (duplicate, job) = self.store.create_job(
            &id,
            optional_str(params, "title"),
            spec,
            spec_sha256,
            &pm,
            optional_str(params, "issue"),
            optional_str(params, "repo"),
            optional_str(params, "base_ref"),
            optional_i64(params, "max_revisions").unwrap_or(2),
            optional_i64(params, "stall_secs"),
            optional_str(params, "task_title"),
            optional_str(params, "task_worktree"),
            optional_str(params, "task_branch"),
            optional_str(params, "task_base_sha"),
            optional_str(params, "task_assignee"),
            optional_str(params, "task_acceptance"),
        )?;
        self.wake();
        Ok(json!({"job": job.to_json(), "duplicate": duplicate}))
    }

    pub(super) fn rpc_job_list(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        // CAD-437: `states` is the repeatable any-of form; the singular
        // `state` stays accepted and merges into it.
        let mut states = optional_strs(params, "states")?;
        if let Some(s) = optional_str(params, "state") {
            states.push(s.to_string());
        }
        states.sort();
        states.dedup();
        check_values("state", &states, crate::store::JOB_STATES)?;
        let (scope, cli_principal, public_principal) =
            self.agent_read_scope(params, peer_pid, "job list")?;
        // CAD-2215: a relay names the reader this list is for. The proof was
        // just verified; the acknowledgement set at the end is added to THIS
        // reply only after the reader's project scope and private-job denials
        // are re-proved there — never borrowed from a preceding probe that a
        // replacement daemon could race.
        let relayed = params.get("public_session").is_some()
            || params.get("read_as").is_some()
            || params.get("cli_envelope").is_some();
        let private_denials = if scope.is_some() {
            Some(self.private_job_denials_for_read(
                params,
                cli_principal.as_ref(),
                public_principal.as_ref(),
                peer_pid,
            )?)
        } else {
            None
        };
        // One state goes to SQL; several load everything and filter —
        // an explicit state set also implies `all` (done/failed/
        // cancelled are terminal rows the default hides).
        let (sql_state, post) = match states.as_slice() {
            [one] => (Some(one.as_str()), false),
            _ => (None, !states.is_empty()),
        };
        let mut jobs = self.store.jobs(
            sql_state,
            params.get("all").and_then(Value::as_bool).unwrap_or(false) || post,
        )?;
        if post {
            jobs.retain(|j| states.contains(&j.state));
        }
        // CAD-325: `tasks_detail` lists each task's id/state/title, so the
        // board binds agents to issues without one `job_show` per job.
        // CAD-1280: `open_tasks_only` leaves terminal tasks out of that
        // list (the board binds only what an agent still holds), and the
        // counts and lists come from one query each, not one per job.
        let detail = params.get("tasks_detail").and_then(Value::as_bool) == Some(true);
        let open_only = params.get("open_tasks_only").and_then(Value::as_bool) == Some(true);
        let mut summary = self.store.job_task_summary(detail.then_some(open_only))?;
        let mut out = Vec::new();
        // The rows this reply actually carries, so the reproof below judges
        // them rather than the reader's whole hidden set.
        let mut returned_jobs: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut returned_tasks: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        // Every issue this reply names, re-admitted under the scope read at
        // reproof: an issue can move to a project the reader cannot see
        // without changing which projects that reader can see.
        let mut returned_issues: Vec<String> = Vec::new();
        for job in jobs {
            if !scope_allows_issue(&scope, job.issue_id.as_deref()) {
                continue;
            }
            // CAD-1380: a member-private job is withheld from every caller
            // except the operator, its owner, an authorized actor, or a
            // subscriber, before it is serialized.
            if private_denials
                .as_ref()
                .is_some_and(|denied| denied.jobs.contains(&job.id))
            {
                continue;
            }
            returned_jobs.insert(job.id.clone());
            if let Some(issue) = job.issue_id.as_deref() {
                returned_issues.push(issue.to_owned());
            }
            let mut j = job.to_json();
            j["tasks"] = json!(summary.counts.remove(&job.id).unwrap_or_default());
            if detail {
                let rows = summary.lists.remove(&job.id).unwrap_or_default();
                for task in &rows {
                    returned_tasks.insert(task.id.clone());
                }
                j["task_list"] = json!(rows
                    .iter()
                    .map(|t| json!({"id": t.id, "state": t.state, "title": t.title}))
                    .collect::<Vec<_>>());
            }
            out.push(j);
        }
        if private_denials.is_some() || public_principal.is_some() {
            #[cfg(test)]
            if let Some(hook) = &self.job_list_snapshot_hook {
                hook("job_list", "");
            }
            let (current_scope, current_cli, current_public) =
                self.agent_read_scope(params, peer_pid, "job list")?;
            // Only the reader's READABLE PROJECT SET can invalidate a row that
            // was already selected: an unrelated issue appearing in the map
            // does not change what this reader may see. Refusing on the whole
            // map would turn a hidden arrival into an error signal.
            let readable_changed = match (&current_scope, &scope) {
                (Some(now), Some(before)) => now.0 != before.0,
                (None, None) => false,
                _ => true,
            };
            // Re-admit exactly the returned rows. A job, task or issue that
            // became private is selected and refuses the page; an unrelated
            // hidden row is not selected and cannot.
            let selected_changed = match &private_denials {
                Some(before) => {
                    let now = self.private_job_denials_for_read(
                        params,
                        current_cli.as_ref(),
                        current_public.as_ref(),
                        peer_pid,
                    )?;
                    // Only the jobs and tasks this reply carries are judged.
                    // The denial set's ISSUES exist so an issue-only message
                    // cannot be attributed to a hidden job; this reply carries
                    // no messages, and a returned job can share an issue with
                    // a hidden sibling without becoming unreadable itself.
                    now.jobs
                        .difference(&before.jobs)
                        .any(|job| returned_jobs.contains(job))
                        || now
                            .tasks
                            .difference(&before.tasks)
                            .any(|task| returned_tasks.contains(task))
                }
                None => false,
            };
            // A carried row's issue must still be readable NOW: `current_scope`
            // was read fresh above, so this is the current issue-to-project
            // mapping, not the entry snapshot. The map keeps every holder of
            // an id, so a hidden project reusing it neither denies nor
            // discloses anything here.
            let carried_unreadable = returned_issues
                .iter()
                .any(|issue| !super::identity::scope_allows_issue(&current_scope, Some(issue)));
            if readable_changed
                || carried_unreadable
                || selected_changed
                || current_public != public_principal
            {
                return Err(Error::rejected("job list refused: reader access changed"));
            }
        }
        let mut out = json!({"jobs": out});
        // CAD-2215: this is the same reply, produced after the reader was
        // re-proved above. An ack only ever means "this answer was narrowed
        // for a non-operator reader": a proven operator (including an
        // operator-role session) and an ordinary local direct caller get the
        // full list and no ack, exactly as before. The field is never part of
        // a client's jobs array; the board strips it.
        if relayed && scope.is_some() {
            out[JOB_SCOPE_FIELD] = json!(JOB_SCOPE_ACK);
        }
        Ok(out)
    }

    /// `job show` — job + tasks with live kickoff state, latest verdict
    /// and lazily-computed drift. No startup reconciliation runs: the
    /// read side flags a task whose kickoff ended without completing,
    /// a dead assignee, a missing SHA, or a spec that drifted since
    /// `job new` hashed it.
    pub(super) fn rpc_job_show(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let id = required_str(params, "job")?;
        let job = self.read_job_for(peer_pid, "job show", id)?;
        let mut jj = job.to_json();
        if let Some(sha) = &job.spec_sha256 {
            match sha256_file(std::path::Path::new(&job.spec_path)) {
                Some(now) if now != *sha => {
                    let canonical_digest = match (job.issue_id.as_deref(), self.pm()) {
                        (Some(issue), Ok(pm)) => {
                            crate::issue::dispatch::canonical_issue_spec_digest(
                                &pm,
                                issue,
                                std::path::Path::new(&job.spec_path),
                                false,
                            )
                        }
                        _ => Ok(None),
                    };
                    match canonical_digest {
                        Ok(Some(now)) if now == *sha => {}
                        Ok(_) => {
                            jj["attention"] = json!(
                                "spec changed since `job new` — \
                                re-create the job if the drift is real work"
                            );
                        }
                        Err(error) => {
                            jj["attention"] = json!(format!(
                                "spec {} is unavailable for drift inspection: {error}",
                                job.spec_path
                            ));
                        }
                    }
                }
                None => {
                    jj["attention"] = json!(format!("spec {} is unreadable", job.spec_path));
                }
                _ => {}
            }
        }
        let scope = self.project_scope(peer_pid, "job show")?;
        let job_tasks = self.store.tasks_for_job(id)?;
        let task_ids = scope.as_ref().map(|_| {
            job_tasks
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>()
        });
        let task_issues = match &task_ids {
            Some(ids) => self.store.task_issue_bindings(ids)?,
            None => Default::default(),
        };
        let mut tasks = Vec::with_capacity(job_tasks.len());
        let mut kickoffs: Vec<String> = Vec::new();
        for task in &job_tasks {
            let (rendered, kickoff) = self.task_json(task, &scope, &task_issues, peer_pid)?;
            tasks.push(rendered);
            kickoffs.extend(kickoff);
        }
        jj["tasks"] = json!(tasks);
        #[cfg(test)]
        if let Some(hook) = &self.job_detail_snapshot_hook {
            hook("job_show", id);
        }
        self.member_assist_policy("job_show", params, peer_pid)?;
        let current = self.read_job_for(peer_pid, "job show", id)?;
        if current.issue_id != job.issue_id
            || self.project_scope(peer_pid, "job show")? != scope
            || match &task_ids {
                Some(ids) => self.store.task_issue_bindings(ids)? != task_issues,
                None => false,
            }
        {
            return Err(Error::rejected("job show refused: reader access changed"));
        }
        self.reprove_task_kickoffs("job show", &kickoffs, &scope, peer_pid)?;
        Ok(json!({"job": jj}))
    }

    /// Re-prove the cloud admission of the kickoff messages the reply served.
    ///
    /// `kickoffs` is what [`Self::task_json`] actually admitted, never the
    /// task rows' dispatch pointers: a kickoff the reader cannot see was
    /// redacted out of the reply, and refusing it here would both break a
    /// readable detail and disclose the hidden kickoff's existence. The
    /// admitted reference carries the message id and state, which is
    /// member-visible metadata, so a cloud grant withdrawn between selection
    /// and the response must not be served from the earlier snapshot. The
    /// operator keeps the unscoped view, as it does for every other read here.
    fn reprove_task_kickoffs(
        &self,
        verb: &str,
        kickoffs: &[String],
        scope: &Option<super::identity::ProjectScope>,
        peer_pid: u32,
    ) -> Result<()> {
        if scope.is_none() {
            return Ok(());
        }
        for mid in kickoffs {
            let Some(message) = self.store.message(mid)? else {
                continue;
            };
            if !self.may_read_cloud_row(
                peer_pid,
                &message.alias,
                &message.id,
                message.sender_uid.as_deref(),
                message.recipient_uid.as_deref(),
            )? {
                return Err(Error::rejected(format!(
                    "{verb} refused: reader access changed"
                )));
            }
        }
        Ok(())
    }

    /// One task rendered for show/list: the row plus live kickoff state,
    /// drift flags and the latest verdict. The second value is the kickoff
    /// message this reply ADMITTED (its `kickoff` block is present), so the
    /// caller re-proves exactly what it served: a kickoff the reader cannot
    /// see is redacted here and must not turn the reply into a refusal that
    /// would disclose it.
    fn task_json(
        self: &Arc<Self>,
        task: &store::Task,
        scope: &Option<super::identity::ProjectScope>,
        task_issues: &std::collections::HashMap<String, Option<String>>,
        peer_pid: u32,
    ) -> Result<(Value, Option<String>)> {
        let mut j = task.to_json();
        let mut admitted_kickoff: Option<String> = None;
        if let Some(mid) = &task.dispatch_message {
            match self.store.message(mid)? {
                Some(m) => {
                    let admitted = scope.is_none()
                        || (m.task_id.as_deref() == Some(task.id.as_str())
                            && super::identity::scope_allows_message(scope, &m, task_issues)
                            && self.may_read_cloud_row(
                                peer_pid,
                                &m.alias,
                                &m.id,
                                m.sender_uid.as_deref(),
                                m.recipient_uid.as_deref(),
                            )?);
                    if !admitted {
                        j["dispatch_message"] = Value::Null;
                    } else {
                        admitted_kickoff = Some(m.id.clone());
                        j["kickoff"] = json!({"id": m.id, "state": m.state});
                        if scope.is_none() {
                            j["kickoff"]["turn_id"] = json!(m.turn_id);
                        }
                        if m.state == "running" {
                            if let Some(assignee) = &task.assignee {
                                if let Some(view) = self.stall_view(assignee) {
                                    view.apply(&mut j);
                                }
                            }
                        }
                        if matches!(task.state.as_str(), "dispatched" | "running")
                            && is_terminal(&m.state)
                            && m.state != "completed"
                        {
                            let assignee = task.assignee.as_deref().unwrap_or("?");
                            j["attention"] = if m.state == "unknown" {
                                json!(unknown_kickoff_attention(mid, assignee))
                            } else {
                                json!(ordinary_terminal_kickoff_attention(
                                    mid,
                                    &m.state,
                                    &task.id,
                                    task.revision + 1,
                                ))
                            };
                        }
                    }
                }
                None if scope.is_some() => j["dispatch_message"] = Value::Null,
                None => {
                    j["attention"] = json!(format!(
                        "dispatch message {mid} is gone — history is incomplete"
                    ));
                }
            }
        }
        if let Some(assignee) = &task.assignee {
            if let Ok(agent) = self.store.agent(assignee) {
                if agent.endpoint.is_none()
                    && registry::has_actor(&agent.provider, &agent.endpoint_kind)
                    && !is_task_terminal(&task.state)
                {
                    j["assignee_dead"] = json!(format!(
                        "assignee {assignee} has no live endpoint — \
                         `cadence job dispatch {} --to <worker>` reassigns",
                        task.id
                    ));
                }
            }
        }
        // The review-without-SHA diagnostic is kickoff-derived: a task's state
        // and head_sha come from its kickoff result, so a reader who was not
        // served that kickoff must not learn what it did or did not report.
        if task.state == "review"
            && task.head_sha.is_none()
            && (scope.is_none() || admitted_kickoff.is_some())
        {
            j["attention"] = json!(format!(
                "kickoff reported no SHA — `cadence job task sha {} <sha>` \
                 records it before a verdict can land",
                task.id
            ));
        }
        let verdicts = self.store.verdicts_for_task(&task.id)?;
        if let Some(v) = store::current_verdict(task.revision, &verdicts) {
            j["latest_verdict"] = super::identity::verdict_json_for_scope(v, scope.is_some());
        }
        Ok((j, admitted_kickoff))
    }

    pub(super) fn rpc_task_show(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let id = required_str(params, "task")?;
        let caller = self.agent_caller(peer_pid, "task show")?;
        let task = self.read_task_for(&caller, id)?;
        let scope = self.project_scope_for_caller(&caller)?;
        let task_issues = if scope.is_some() {
            self.store.task_issue_bindings(&[task.id.as_str()])?
        } else {
            Default::default()
        };
        let (mut out, kickoff) = self.task_json(&task, &scope, &task_issues, peer_pid)?;
        let messages = self.store.messages_for_task(&task.id)?;
        let mut visible = Vec::with_capacity(messages.len());
        let mut selected: Vec<&store::Message> = Vec::new();
        for message in &messages {
            if scope.is_some()
                && (!super::identity::scope_allows_message(&scope, message, &task_issues)
                    || !self.may_read_cloud_row(
                        peer_pid,
                        &message.alias,
                        &message.id,
                        message.sender_uid.as_deref(),
                        message.recipient_uid.as_deref(),
                    )?)
            {
                continue;
            }
            selected.push(message);
            visible.push(super::identity::message_json_for_scope(message, &scope));
        }
        out["messages"] = json!(visible);
        out["verdicts"] = json!(self
            .store
            .verdicts_for_task(&task.id)?
            .iter()
            .map(|verdict| super::identity::verdict_json_for_scope(verdict, scope.is_some()))
            .collect::<Vec<_>>());
        #[cfg(test)]
        if let Some(hook) = &self.job_detail_snapshot_hook {
            hook("task_show", id);
        }
        self.member_assist_policy("task_show", params, peer_pid)?;
        let current_caller = self.agent_caller(peer_pid, "task show")?;
        let current = self.read_task_for(&current_caller, id)?;
        if current_caller != caller
            || current.job_id != task.job_id
            || self.project_scope_for_caller(&current_caller)? != scope
            || (scope.is_some()
                && self.store.task_issue_bindings(&[task.id.as_str()])? != task_issues)
        {
            return Err(Error::rejected("task show refused: reader access changed"));
        }
        // The reply carries the bodies of every admitted message, so re-prove
        // that each one is still admitted now: a cloud grant withdrawn between
        // selection and this response must not serve the earlier snapshot.
        if scope.is_some() {
            for message in &selected {
                if !self.may_read_cloud_row(
                    peer_pid,
                    &message.alias,
                    &message.id,
                    message.sender_uid.as_deref(),
                    message.recipient_uid.as_deref(),
                )? {
                    return Err(Error::rejected("task show refused: reader access changed"));
                }
            }
        }
        // The kickoff is re-proved on its own even though the body loop above
        // re-proves the same message: `task_json` admits it first, and a grant
        // withdrawn between that admission and the loop drops the message from
        // `selected` without taking back the `kickoff` block already built.
        self.reprove_task_kickoffs(
            "task show",
            &kickoff.into_iter().collect::<Vec<_>>(),
            &scope,
            peer_pid,
        )?;
        Ok(json!({"task": out}))
    }

    pub(super) fn rpc_job_events(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let id = required_str(params, "job")?;
        // CAD-2215: the request shape is settled BEFORE any job is named or
        // any event is read, so a downgrade attempt learns nothing.
        let format = job_events_opaque::requested_format(params, "job events")?;
        if format.is_some_and(|format| format != job_events_opaque::FORMAT) {
            return Err(job_events_opaque::format_required("job events"));
        }
        let after = job_events_opaque::supplied_after(params);
        let reader = self.verified_job_event_reader(params, peer_pid, "job events")?;
        // A relayed scoped reader is the proof the request carried, never the
        // board connection: it is admitted by its project scope and the
        // private-job denials that proof resolves to. A non-operator direct
        // caller keeps the same connection-bound check it always had, and a
        // proven operator keeps the unscoped view.
        let scoped = reader.principal.is_some();
        // A relayed reader's proof is what the request carried, so its
        // private-job boundary is the denials THAT reader resolves to. A
        // direct caller keeps its connection-bound `member_assist_policy`
        // target proof, which is exact for this job, instead of the wider
        // principal scan.
        let denials = if scoped && reader.relayed {
            Some(self.private_job_denials_for_read(
                params,
                reader.cli.as_ref(),
                reader.public.as_ref(),
                peer_pid,
            )?)
        } else {
            None
        };
        let job = if scoped && reader.relayed {
            // A relayed reader is admitted by the authority the request
            // proved, never by the board connection that carried it.
            let job = self.store.job(id)?;
            if !scope_allows_issue(&reader.scope, job.issue_id.as_deref())
                || denials
                    .as_ref()
                    .is_some_and(|denied| denied.jobs.contains(&job.id))
            {
                return Err(Error::rejected(format!("No such job '{id}'")));
            }
            job
        } else {
            // Every direct caller (and every proven operator) keeps the
            // exact connection-bound admission it has always had.
            self.read_job_for(peer_pid, "job events", id)?
        };
        // A scoped reader must continue with a token. A number is a
        // downgrade and an unknown/absent format is refused — never coerced
        // and never served as numeric positions.
        let supplied = if !scoped {
            None
        } else {
            if format.is_none() {
                return Err(job_events_opaque::format_required("job events"));
            }
            match after {
                job_events_opaque::SuppliedAfter::Absent => None,
                job_events_opaque::SuppliedAfter::Text(token) => Some(token),
                job_events_opaque::SuppliedAfter::Number => {
                    return Err(job_events_opaque::numeric_cursor("job events"))
                }
                job_events_opaque::SuppliedAfter::Unsupported => {
                    return Err(job_events_opaque::invalid_after("job events"))
                }
            }
        };
        // The legacy numeric continuation, preserved exactly for the operator.
        let numeric_after = params.get("after").and_then(Value::as_i64).unwrap_or(0);
        if numeric_after < 0 {
            return Err(Error::rejected("Event cursor must be nonnegative"));
        }
        // Missing key material refuses a scoped read before any content; it
        // never downgrades to numeric positions.
        let codec = if scoped {
            Some(self.scoped_cursor_codec()?)
        } else {
            None
        };
        let position = match (&codec, &reader.principal, supplied) {
            (Some(codec), Some(principal), Some(token)) => Some(job_events_opaque::open(
                codec,
                principal,
                &job,
                token,
                "job events",
            )?),
            _ => None,
        };
        self.member_assist_policy("job_events", params, peer_pid)?;
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        // Same `tail` contract as agent_events — the job view's
        // default page is the newest too.
        if params.get("tail").and_then(Value::as_bool).unwrap_or(false) {
            let events = self.store.job_events_tail(&job.id, 51)?;
            let has_older = events.len() > 50;
            // The store returns oldest first; omit the extra row without
            // shifting the retained events.
            let events = &events[usize::from(has_older)..];
            #[cfg(test)]
            if let Some(hook) = &self.job_events_snapshot_hook {
                hook(id);
            }
            self.reprove_job_event_read(params, peer_pid, &reader, &job, &denials)?;
            let mut out = match (&codec, &reader.principal) {
                (Some(codec), Some(principal)) => job_events_opaque::scoped_page(
                    codec,
                    principal,
                    &job,
                    events,
                    // A tail page ignores `after`; its cursor is the last
                    // visible row or null, never a re-echoed token.
                    None,
                    Some(has_older),
                    "job events",
                )?,
                _ => json!({
                    "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
                    "cursor": events.last().map(|e| e.seq).unwrap_or(0),
                    "has_older": has_older,
                }),
            };
            Self::mark_job_event_reader_scope(&mut out, &reader, format.is_some());
            return Ok(out);
        }
        let deadline = Instant::now() + Duration::from_secs(wait);
        let start = position.unwrap_or(numeric_after);
        loop {
            let events = self.store.job_events(&job.id, start, 200)?;
            if !events.is_empty()
                || self.closing.load(Ordering::SeqCst)
                || Instant::now() >= deadline
            {
                #[cfg(test)]
                if let Some(hook) = &self.job_events_snapshot_hook {
                    hook(id);
                }
                self.reprove_job_event_read(params, peer_pid, &reader, &job, &denials)?;
                let mut out = match (&codec, &reader.principal) {
                    (Some(codec), Some(principal)) => job_events_opaque::scoped_page(
                        codec,
                        principal,
                        &job,
                        &events,
                        supplied,
                        None,
                        "job events",
                    )?,
                    _ => json!({
                        "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
                        "cursor": events.last().map(|e| e.seq).unwrap_or(start),
                    }),
                };
                Self::mark_job_event_reader_scope(&mut out, &reader, format.is_some());
                return Ok(out);
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// CAD-2215: the per-response reader acknowledgement. A scoped page always
    /// names its reader scope; a proven operator names it only when it asked
    /// for the new format, because its answer is the legacy numeric document
    /// and its absence is the unchanged legacy reply.
    fn mark_job_event_reader_scope(
        out: &mut Value,
        reader: &job_events_opaque::VerifiedJobEventReader,
        operator_format: bool,
    ) {
        if reader.principal.is_some() {
            out[READER_SCOPE_FIELD] = json!(JOB_EVENTS_SCOPE_ACK);
        } else if operator_format {
            out[READER_SCOPE_FIELD] = json!(JOB_EVENTS_OPERATOR_SCOPE_ACK);
        }
    }

    /// CAD-2215: re-prove every input the selected page depended on AFTER
    /// selection and before any content leaves — the connection's caller rule
    /// (or the relayed reader's live proof), its project scope, the
    /// member/private-job boundary (`member_assist_policy` re-runs the
    /// target-side guard), the relayed reader's private-job denials, and the
    /// job's stored id and creation timestamp. Any change is one refusal,
    /// never a mixed snapshot.
    fn reprove_job_event_read(
        &self,
        params: &Value,
        peer_pid: u32,
        reader: &job_events_opaque::VerifiedJobEventReader,
        job: &store::Job,
        denials: &Option<store::PrivateJobDenials>,
    ) -> Result<()> {
        let changed = || Error::rejected("job events refused: reader access changed");
        self.member_assist_policy("job_events", params, peer_pid)?;
        let current = self.verified_job_event_reader(params, peer_pid, "job events")?;
        if !reader.same_authority(&current) {
            return Err(changed());
        }
        if let Some(previous) = denials {
            if self.private_job_denials_for_read(
                params,
                current.cli.as_ref(),
                current.public.as_ref(),
                peer_pid,
            )? != *previous
            {
                return Err(changed());
            }
        }
        let current_job = self.store.job(&job.id).map_err(|_| changed())?;
        if current_job.issue_id != job.issue_id
            || job_events_opaque::job_resource(&current_job) != job_events_opaque::job_resource(job)
        {
            return Err(changed());
        }
        if current.relayed && current.principal.is_some() {
            if !scope_allows_issue(&current.scope, current_job.issue_id.as_deref()) {
                return Err(Error::rejected(format!("No such job '{}'", job.id)));
            }
        } else {
            self.read_job_for(peer_pid, "job events", &job.id)?;
        }
        Ok(())
    }

    pub(super) fn rpc_task_new(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let job = self.store.job(required_str(params, "job")?)?;
        let id = match optional_str(params, "task") {
            Some(id) => id.to_string(),
            None => {
                // Default `<job>-t<n>` — first free index keeps ids
                // readable and collision-free.
                let existing = self.store.tasks_for_job(&job.id)?;
                let mut n = existing.len() + 1;
                loop {
                    let candidate = format!("{}-t{n}", job.id);
                    if self.store.task_opt(&candidate)?.is_none() {
                        break candidate;
                    }
                    n += 1;
                }
            }
        };
        let assignee = optional_str(params, "assignee")
            .map(|a| self.resolve_alias(a))
            .transpose()?;
        let task = self.store.create_task(
            &job.id,
            &id,
            optional_str(params, "title"),
            assignee.as_deref(),
            optional_str(params, "spec"),
            optional_str(params, "acceptance"),
            optional_str(params, "worktree"),
            optional_str(params, "branch"),
            optional_str(params, "base_sha"),
        )?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    /// Read-only entitlement probe for a cloud start. The CLI has no
    /// hosted company context; the daemon does, and its refusal carries
    /// the stable `cloud_sandbox_not_entitled` code.
    pub(super) fn rpc_cloud_probe(self: &Arc<Self>, _params: &Value) -> Result<Value> {
        crate::issue::cloud::probe_entitlement()?;
        Ok(json!({"entitled": true}))
    }

    /// CAD-622: the app-team admission every fresh native kickoff shares.
    /// `task_dispatch` and both monitor paths commit their kickoff through
    /// `dispatch_task_planned`, never through `dispatch_send`, so they
    /// need the same store-backed guard here: the caller's claim, the
    /// job's own PM and the planned worker are all authors of the work
    /// the kickoff opens; the ticket's planned paths and the repository
    /// the kickoff actually writes to are the evidence. A refusal leaves
    /// the task, the job and the queue untouched.
    ///
    /// The tracker is opened only once the job proves it is bound to an
    /// issue: a standalone job keeps working without one.
    pub(super) fn native_app_guard(
        &self,
        plan: &crate::store::DispatchPlan,
        caller: Option<&str>,
    ) -> Result<()> {
        let Some(issue_id) = plan.job.issue_id.as_deref() else {
            return Ok(());
        };
        let pm = self.pm()?;
        let Some(gated) = crate::issue::dispatch::capture_issue(&pm, issue_id)? else {
            return Ok(());
        };
        let app_paths =
            crate::issue::app_guard::apps_in_paths(gated.front.paths.iter().map(String::as_str));
        let mut authors: Vec<&str> = vec![plan.worker.alias.as_str(), plan.job.pm_alias.as_str()];
        authors.extend(caller.filter(|c| !c.is_empty()));
        let guard = crate::issue::app_guard::Ctx {
            pm_dir: &pm.dir,
            state_dir: Some(&self.state_dir),
            store: Some(&self.store),
        };
        // Nothing to check is not the same answer as "could not check":
        // only a change that names an app, or whose author is on an app's
        // team, pays for — and demands — the repository proof below.
        if !crate::issue::app_guard::app_question_live(guard, &app_paths, &authors)? {
            return Ok(());
        }
        // The repository the kickoff writes to is the task's own worktree —
        // the place the kickoff tells the worker to work — normalized to
        // its main checkout, so a linked worktree names the same
        // repository as its main checkout. `job.repo` is separate,
        // optional metadata: a claim that disagrees is unprovable, never
        // an empty app set.
        let worktree_root = match plan.task.worktree.as_deref() {
            Some(worktree) => {
                let dir = std::path::Path::new(worktree);
                Some(crate::worktree::main_root(dir).map_err(|e| {
                    Error::rejected(format!(
                        "task dispatch refused: task '{}' binds worktree {} whose \
                         repository cannot be proven ({e}) — the app-team check cannot \
                         be made",
                        plan.task.id,
                        dir.display()
                    ))
                })?)
            }
            None => None,
        };
        let job_root = plan.job.repo.as_deref().map(|repo| {
            let dir = std::path::Path::new(repo);
            crate::worktree::main_root(dir)
                .or_else(|_| dir.canonicalize())
                .unwrap_or_else(|_| dir.to_path_buf())
        });
        let root = match (worktree_root, job_root) {
            (Some(worktree), Some(job)) if worktree != job => {
                return Err(Error::rejected(format!(
                    "task dispatch refused: job '{}' names repository {}, but task '{}' \
                     binds worktree {} in {} — the app-team check cannot be made on two \
                     repositories",
                    plan.job.id,
                    job.display(),
                    plan.task.id,
                    plan.task.worktree.as_deref().unwrap_or_default(),
                    worktree.display()
                )));
            }
            (Some(root), _) => Some(root),
            (None, job) => job,
        };
        let Some(root) = root else {
            return Err(Error::rejected(format!(
                "task dispatch refused: job '{}' names no repository and task '{}' binds \
                 no worktree, so which repository this kickoff writes to cannot be proven \
                 — the app-team check for {issue_id} cannot be made",
                plan.job.id, plan.task.id
            )));
        };
        // The checkout's own source identity, not only the project's
        // registered paths: a task bound to another local checkout of the
        // app's pinned git source is matched by its own origin, and an
        // unprovable checkout refuses instead of becoming an empty app
        // set.
        let remotes =
            crate::issue::app_guard::checkout_remotes(&gated.project, &root).map_err(|e| {
                Error::rejected(format!(
                    "task dispatch refused: job '{}' / task '{}' — {}",
                    plan.job.id, plan.task.id, e
                ))
            })?;
        match crate::issue::app_guard::refusals(guard, &app_paths, &authors, &remotes)? {
            Some(why) => Err(Error::rejected(why)),
            None => Ok(()),
        }
    }

    /// The settled identity of one member-private job. `Ok(None)` means
    /// the job is not member-private. A member-private job whose owner is
    /// not the globally settled queue+ledger provenance, whose ticket does
    /// not resolve uniquely, or whose project is unreadable is surfaced —
    /// never read as "not private".
    fn private_job_context(&self, job_id: &str) -> Result<Option<PrivateJobContext>> {
        let job = self.store.job(job_id).map_err(|e| {
            Error::rejected(format!(
                "private delivery: job {job_id} cannot be read ({e})"
            ))
        })?;
        if !self.store.assist_job_has_request(job_id)? {
            return Ok(None);
        }
        let request = self.store.assist_request_for_job(job_id)?.ok_or_else(|| {
            Error::rejected(format!(
                "private delivery: job {job_id} is member-private but has no unique request \
                 owner — operator reconciliation is required"
            ))
        })?;
        // The owner must be the GLOBALLY settled queue+ledger provenance,
        // not merely one ledger row: a second principal's queue claim, a
        // half-completed bind or a mismatched planned job makes the owner
        // ambiguous and is a refusal.
        let Some((owner_org, owner_sub)) = self.store.assist_job_owner(job_id)? else {
            return Err(Error::rejected(format!(
                "private delivery: job {job_id} has no settled single owner — operator \
                 reconciliation is required"
            )));
        };
        if owner_org != request.org || owner_sub != request.sub {
            return Err(Error::rejected(format!(
                "private delivery: job {job_id}'s settled owner disagrees with its request"
            )));
        }
        let issue = job
            .issue_id
            .clone()
            .filter(|issue| !issue.is_empty())
            .ok_or_else(|| {
                Error::rejected(format!(
                    "private delivery: job {job_id} is member-private but not bound to a ticket"
                ))
            })?;
        let pm = self.pm()?;
        let (project, _) = crate::issue::write::issue_dir(&pm, &issue).map_err(|_| {
            Error::rejected(format!(
                "private delivery: job {job_id}'s ticket {issue} is not uniquely resolvable"
            ))
        })?;
        Ok(Some(PrivateJobContext {
            job,
            request,
            issue,
            project: project.key,
        }))
    }

    /// Prove one exact dispatch of a member-private job from the durable
    /// `task_dispatched` row and the kickoff message. The project is the
    /// one bound at dispatch and must still be the ticket's project: a
    /// moved ticket refuses rather than rebinding.
    fn private_dispatch_binding(
        &self,
        ctx: &PrivateJobContext,
        task_id: &str,
        dispatch: &str,
        events: &[store::TaskHistoryEvent],
    ) -> Result<PrivateDispatchBinding> {
        let mut dispatches = events.iter().filter(|event| {
            event.kind == "task_dispatched" && event.message.as_deref() == Some(dispatch)
        });
        let Some(event) = dispatches.next() else {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch} has no task_dispatched event"
            )));
        };
        if dispatches.next().is_some() {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch} has duplicate task_dispatched events"
            )));
        }
        let (Some(revision), Some(assignee), Some(project)) = (
            event.revision,
            event
                .assignee
                .as_deref()
                .filter(|alias| !alias.trim().is_empty()),
            event
                .project
                .as_deref()
                .filter(|key| !key.trim().is_empty()),
        ) else {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch}'s task_dispatched event is incomplete"
            )));
        };
        if event.task.as_deref() != Some(task_id)
            || event.job.as_deref() != Some(ctx.job.id.as_str())
        {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch}'s event names another task or job"
            )));
        }
        if project != ctx.project.as_str() {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch}'s project differs from the ticket's"
            )));
        }
        let message = self.store.message(dispatch)?.ok_or_else(|| {
            Error::rejected(format!(
                "private delivery: the recorded dispatch message {dispatch} is missing"
            ))
        })?;
        if message.source != "job_dispatch"
            || message.task_id.as_deref() != Some(task_id)
            || message.alias != assignee
            || message.issue.as_deref() != Some(ctx.issue.as_str())
        {
            return Err(Error::rejected(format!(
                "private delivery: the dispatch message {dispatch} does not bind the task, \
                 worker and issue"
            )));
        }
        Ok(PrivateDispatchBinding {
            message,
            revision,
            assignee: assignee.to_string(),
            project: project.to_string(),
        })
    }

    /// A binding plus the canonical typed report SHA and the exact stored
    /// result body. Any missing, duplicated or mismatched report evidence
    /// is a refusal — the mutable `task.head_sha` is never consulted.
    fn private_dispatch_proof(
        &self,
        ctx: &PrivateJobContext,
        task_id: &str,
        binding: PrivateDispatchBinding,
        events: &[store::TaskHistoryEvent],
    ) -> Result<PrivateDispatchProof> {
        let dispatch = binding.message.id.clone();
        if binding.message.state != "completed" {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch} is not complete"
            )));
        }
        let Some(result) = binding.message.result.as_ref() else {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch} carries no durable result"
            )));
        };
        let sha = Self::recorded_report_sha(
            events,
            &ctx.job.id,
            task_id,
            binding.revision,
            &dispatch,
            &binding.assignee,
        )?;
        if let Some(result_sha) = result.get("sha").and_then(Value::as_str) {
            if Some(result_sha) != sha.as_deref() {
                return Err(Error::rejected(format!(
                    "private delivery: dispatch {dispatch}'s result sha disagrees with the \
                     recorded task_reported evidence"
                )));
            }
        }
        let body = Self::private_result_body(result);
        if body.is_empty() {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch} carries no durable result text"
            )));
        }
        Ok(PrivateDispatchProof { binding, sha, body })
    }

    /// The live-path checks that use the mutable task row: the dispatch
    /// evidence must still describe this task's current revision, worker
    /// and worktree. Historical recovery never calls this — an old
    /// dispatch proves entirely from its own rows.
    fn private_live_binding_checks(
        &self,
        task: &store::Task,
        dispatch: &str,
        binding: &PrivateDispatchBinding,
    ) -> Result<()> {
        if binding.revision != task.revision {
            return Err(Error::rejected(format!(
                "private delivery: task {}'s revision changed while reading",
                task.id
            )));
        }
        if binding.assignee != task.assignee.as_deref().unwrap_or_default() {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch}'s worker is not the task's assignee"
            )));
        }
        if binding.message.worktree.as_deref() != task.worktree.as_deref() {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch} does not bind the task's worktree"
            )));
        }
        Ok(())
    }

    /// CAD-2215: the exact private origin of one assist-bound task,
    /// derived only from the settled job/request owner and the native
    /// dispatch evidence. `Ok(None)` means the job is not member-private.
    fn private_origin(
        &self,
        task: &store::Task,
    ) -> Result<Option<(crate::delivery::private::VerifiedJobOrigin, store::Message)>> {
        let Some(dispatch) = task
            .dispatch_message
            .as_deref()
            .filter(|dispatch| !dispatch.is_empty())
        else {
            return Ok(None);
        };
        let Some(ctx) = self.private_job_context(&task.job_id)? else {
            return Ok(None);
        };
        let events = self.store.task_history_events(&task.job_id, &task.id)?;
        let binding = self.private_dispatch_binding(&ctx, &task.id, dispatch, &events)?;
        self.private_live_binding_checks(task, dispatch, &binding)?;
        let origin = crate::delivery::private::VerifiedJobOrigin::for_dispatch(
            &ctx.job,
            &task.id,
            binding.revision,
            dispatch,
            &ctx.issue,
            &ctx.request,
            &binding.project,
        )?;
        Ok(Some((origin, binding.message)))
    }

    /// Re-prove one recorded private delivery from native rows before any
    /// shared publication: the settled single owner, the job's current
    /// issue/project, the exact historical dispatch message with its
    /// unique dispatch/report evidence, and the stored result's canonical
    /// SHA, exact body and digest. A reopened/re-dispatched task proves
    /// through its own dispatch rows, never the mutable task pointer.
    /// Read-only; missing, duplicated or drifted evidence is a refusal.
    pub(super) fn verify_private_delivery_origin(
        &self,
        delivery: &crate::delivery::private::PrivateDelivery,
    ) -> Result<()> {
        let origin = &delivery.origin;
        if delivery.id != origin.delivery_id() {
            return Err(Error::rejected(
                "private delivery: the recorded id does not name its own origin",
            ));
        }
        let ctx = self
            .private_job_context(&origin.job)?
            .ok_or_else(|| Error::rejected("private delivery: the job is not member-private"))?;
        let task = match self.store.task(&origin.task) {
            Ok(task) => task,
            Err(Error::Rejected(_)) => {
                return Err(Error::rejected(
                    "private delivery: the recorded task is missing",
                ))
            }
            Err(error) => return Err(error),
        };
        if task.job_id != origin.job {
            return Err(Error::rejected(
                "private delivery: the recorded task belongs to another job",
            ));
        }
        let events = self.store.task_history_events(&origin.job, &origin.task)?;
        let binding =
            self.private_dispatch_binding(&ctx, &origin.task, &origin.dispatch, &events)?;
        let proof = self.private_dispatch_proof(&ctx, &origin.task, binding, &events)?;
        let expected = crate::delivery::private::VerifiedJobOrigin::for_dispatch(
            &ctx.job,
            &origin.task,
            proof.binding.revision,
            &origin.dispatch,
            &ctx.issue,
            &ctx.request,
            &proof.binding.project,
        )?;
        // `authority_rev` is dispatch-time evidence. An explicit member job
        // reconcile may legitimately change the request's stored revision
        // later, and the dispatch event does not record it, so the stored
        // value is preserved but never compared. Every re-provable field
        // must still match exactly, and an unverifiable one is a refusal.
        let drifted = expected.job != origin.job
            || expected.task != origin.task
            || expected.revision != origin.revision
            || expected.dispatch != origin.dispatch
            || expected.issue != origin.issue
            || expected.project != origin.project
            || expected.owner_org != origin.owner_org
            || expected.owner_sub != origin.owner_sub;
        if drifted {
            return Err(Error::rejected(
                "private delivery: the recorded origin drifted from native evidence",
            ));
        }
        if delivery.state != crate::delivery::private::PrivateState::Completed
            || delivery.results.len() != 1
        {
            return Err(Error::rejected(
                "private delivery: exactly one completed result is required to authorize sharing",
            ));
        }
        let result = &delivery.results[0];
        if result.message != origin.dispatch
            || result.revision != origin.revision
            || result.sha != proof.sha
            || result.body != proof.body
            || result.digest
                != crate::delivery::private::result_digest(
                    &result.message,
                    proof.sha.as_deref(),
                    &result.body,
                )
        {
            return Err(Error::rejected(
                "private delivery: the recorded result drifted from native evidence",
            ));
        }
        Ok(())
    }

    /// Re-prove one still-Running private delivery from native rows before
    /// any projection shows it: the settled single owner, the exact
    /// dispatch/job/task/revision/worker/issue/project binding, and a
    /// message that is genuinely pre-completion. A moved or deleted task,
    /// a moved/malformed dispatch message, a completed/failed/fenced
    /// message, or a row that already holds a result is a refusal — so a
    /// stale readable Running row can never evade native-completion
    /// enumeration. Read-only. As with the completed verifier, the
    /// captured `authority_rev` is preserved and never compared against a
    /// later mutable revision.
    pub(super) fn verify_private_running_origin(
        &self,
        delivery: &crate::delivery::private::PrivateDelivery,
    ) -> Result<()> {
        let origin = &delivery.origin;
        if delivery.id != origin.delivery_id() {
            return Err(Error::rejected(
                "private delivery: the recorded id does not name its own origin",
            ));
        }
        if delivery.state != crate::delivery::private::PrivateState::Running
            || !delivery.results.is_empty()
        {
            return Err(Error::rejected(
                "private delivery: a running delivery must hold no completed result",
            ));
        }
        let ctx = self
            .private_job_context(&origin.job)?
            .ok_or_else(|| Error::rejected("private delivery: the job is not member-private"))?;
        let task = match self.store.task(&origin.task) {
            Ok(task) => task,
            Err(Error::Rejected(_)) => {
                return Err(Error::rejected(
                    "private delivery: the recorded task is missing",
                ))
            }
            Err(error) => return Err(error),
        };
        if task.job_id != origin.job {
            return Err(Error::rejected(
                "private delivery: the recorded task belongs to another job",
            ));
        }
        let events = self.store.task_history_events(&origin.job, &origin.task)?;
        let binding =
            self.private_dispatch_binding(&ctx, &origin.task, &origin.dispatch, &events)?;
        let expected = crate::delivery::private::VerifiedJobOrigin::for_dispatch(
            &ctx.job,
            &origin.task,
            binding.revision,
            &origin.dispatch,
            &ctx.issue,
            &ctx.request,
            &binding.project,
        )?;
        // `authority_rev` is dispatch-time evidence; a later member job
        // reconcile may legitimately change the request, so only the
        // re-provable fields are compared.
        let drifted = expected.job != origin.job
            || expected.task != origin.task
            || expected.revision != origin.revision
            || expected.dispatch != origin.dispatch
            || expected.issue != origin.issue
            || expected.project != origin.project
            || expected.owner_org != origin.owner_org
            || expected.owner_sub != origin.owner_sub;
        if drifted {
            return Err(Error::rejected(
                "private delivery: the recorded origin drifted from native evidence",
            ));
        }
        if !matches!(
            binding.message.state.as_str(),
            "queued" | "submitting" | "running"
        ) {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {} is '{}', not a running kickoff",
                origin.dispatch, binding.message.state
            )));
        }
        Ok(())
    }

    /// The native expectation of one completed private kickoff, computed
    /// exactly as recording does but without writing: the proven origin
    /// and the `PrivateResult` with the exact body, the canonical typed
    /// report SHA and its digest. Valid for the current dispatch and for
    /// an older dispatch after a reopen/re-dispatch, because it proves
    /// from the dispatch's own rows and never the task pointer.
    ///
    /// `PrivateResult::at` is a placeholder `0`: this is an expectation,
    /// not a recorded row, so callers comparing against a ledger entry
    /// must ignore the timestamp. The origin carries the request's CURRENT
    /// `authority_rev`; a later member job reconcile may legitimately
    /// change it, so a consumer must compare the stable provenance fields
    /// (job/task/revision/dispatch/issue/project/owners), not
    /// `authority_rev`.
    pub(super) fn expected_private_dispatch_result(
        &self,
        job_id: &str,
        task: &store::Task,
        message: &store::Message,
    ) -> Result<(
        crate::delivery::private::VerifiedJobOrigin,
        crate::delivery::private::PrivateResult,
    )> {
        if task.job_id != job_id {
            return Err(Error::rejected(
                "private delivery: the task does not belong to the named job",
            ));
        }
        let ctx = self
            .private_job_context(job_id)?
            .ok_or_else(|| Error::rejected("private delivery: the job is not member-private"))?;
        let events = self.store.task_history_events(job_id, &task.id)?;
        let binding = self.private_dispatch_binding(&ctx, &task.id, &message.id, &events)?;
        let proof = self.private_dispatch_proof(&ctx, &task.id, binding, &events)?;
        let origin = crate::delivery::private::VerifiedJobOrigin::for_dispatch(
            &ctx.job,
            &task.id,
            proof.binding.revision,
            &message.id,
            &ctx.issue,
            &ctx.request,
            &proof.binding.project,
        )?;
        let result = crate::delivery::private::PrivateResult {
            message: message.id.clone(),
            revision: proof.binding.revision,
            digest: crate::delivery::private::result_digest(
                &message.id,
                proof.sha.as_deref(),
                &proof.body,
            ),
            sha: proof.sha,
            body: proof.body,
            at: 0,
        };
        Ok((origin, result))
    }

    /// Open (idempotently) the ledger entry for a proven origin, keeping
    /// the authority revision captured at the original dispatch. An
    /// explicit member job reconcile may legitimately change the request's
    /// current revision, and that must not invalidate the stored evidence
    /// for the same delivery; every other field is still compared by
    /// [`crate::delivery::private::open`].
    fn open_private_ledger(
        &self,
        mut origin: crate::delivery::private::VerifiedJobOrigin,
    ) -> Result<()> {
        let id = origin.delivery_id();
        if let Some(existing) = crate::delivery::private::load(&self.state_dir)?.get(&id) {
            origin.authority_rev = existing.origin.authority_rev.clone();
        }
        crate::delivery::private::open(&self.state_dir, origin, crate::issue::time::now_epoch())?;
        Ok(())
    }

    /// Open (idempotently) the private ledger entry for a task. `Ok(None)`
    /// means not member-private; ledger I/O errors surface.
    fn open_private_delivery(&self, task: &store::Task) -> Result<Option<()>> {
        let Some((origin, _)) = self.private_origin(task)? else {
            return Ok(None);
        };
        self.open_private_ledger(origin)?;
        Ok(Some(()))
    }

    /// The one canonical SHA of a dispatch's durable result: the exact
    /// typed `sha` the producer wrote in the matching `task_reported`
    /// event. `None` is the explicit null a SHA-less completion records;
    /// a missing, duplicated, malformed or mismatched event is a refusal.
    /// The task's `head_sha` is mutable (`job task sha` repairs it after
    /// the report) and is never substituted for report evidence.
    fn recorded_report_sha(
        events: &[store::TaskHistoryEvent],
        job: &str,
        task: &str,
        revision: i64,
        dispatch: &str,
        assignee: &str,
    ) -> Result<Option<String>> {
        let mut reports = events.iter().filter(|event| {
            event.kind == "task_reported" && event.message.as_deref() == Some(dispatch)
        });
        let Some(report) = reports.next() else {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch} has no task_reported event"
            )));
        };
        if reports.next().is_some() {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch} has duplicate task_reported events"
            )));
        }
        if report.sha_bad {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch}'s task_reported sha is malformed"
            )));
        }
        if report.task.as_deref() != Some(task)
            || report.job.as_deref() != Some(job)
            || report.revision != Some(revision)
            || report.alias != assignee
        {
            return Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch}'s task_reported event disagrees on \
                 task/job/worker/revision"
            )));
        }
        match report.sha.as_ref().map(|sha| sha.as_deref()) {
            None => Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch}'s task_reported event has no sha field"
            ))),
            Some(None) => Ok(None),
            Some(Some(sha)) if sha.trim().is_empty() => Err(Error::rejected(format!(
                "private delivery: dispatch {dispatch}'s task_reported sha is empty"
            ))),
            Some(Some(sha)) => Ok(Some(sha.to_string())),
        }
    }

    /// The durable result text of one completed message. A reported turn
    /// records `text`; an operator reconcile records the same evidence
    /// under `note` (the field the store's own SHA parsing reads). The
    /// first field with non-blank content wins, but the value recorded
    /// and digested is the EXACT stored string — trimming is used only to
    /// decide that the field carries a body, never to alter its bytes.
    /// Live recording and historical recovery therefore agree exactly.
    fn private_result_body(result: &Value) -> String {
        ["text", "note"]
            .iter()
            .filter_map(|field| result.get(*field).and_then(Value::as_str))
            .find(|body| !body.trim().is_empty())
            .unwrap_or_default()
            .to_string()
    }

    /// Bind the task's durable dispatch result to its private delivery.
    /// A missing ledger entry (transient open failure, a crash after the
    /// dispatch committed, or a restart) is re-opened from the same
    /// native rows before the exact result is appended. Repeated calls
    /// are idempotent and the task is never rerun.
    ///
    /// `Ok(false)` means not member-private or no durable result yet;
    /// `Err` is an unavailable/unknown private ledger and must never be
    /// reported as success.
    fn record_private_result(&self, task: &store::Task) -> Result<bool> {
        if task.state == "draft" && task.revision == 0 {
            return Ok(false);
        }
        // An undispatched draft has no private delivery and no result.
        let Some(dispatch) = task
            .dispatch_message
            .as_deref()
            .filter(|dispatch| !dispatch.is_empty())
        else {
            return Ok(false);
        };
        let Some(ctx) = self.private_job_context(&task.job_id)? else {
            return Ok(false);
        };
        let events = self.store.task_history_events(&task.job_id, &task.id)?;
        let binding = self.private_dispatch_binding(&ctx, &task.id, dispatch, &events)?;
        self.private_live_binding_checks(task, dispatch, &binding)?;
        if binding.message.state != "completed" {
            return Ok(false);
        }
        let proof = self.private_dispatch_proof(&ctx, &task.id, binding, &events)?;
        let origin = crate::delivery::private::VerifiedJobOrigin::for_dispatch(
            &ctx.job,
            &task.id,
            proof.binding.revision,
            dispatch,
            &ctx.issue,
            &ctx.request,
            &proof.binding.project,
        )?;
        let id = origin.delivery_id();
        self.open_private_ledger(origin)?;
        let result = crate::delivery::private::PrivateResult {
            message: dispatch.to_string(),
            revision: proof.binding.revision,
            digest: crate::delivery::private::result_digest(
                dispatch,
                proof.sha.as_deref(),
                &proof.body,
            ),
            sha: proof.sha,
            body: proof.body,
            at: crate::issue::time::now_epoch(),
        };
        crate::delivery::private::append_result(&self.state_dir, &id, result)?;
        Ok(true)
    }

    /// Settle a private delivery that produced no result. Opens a missing
    /// entry first so the persisted failure is not lost.
    fn fail_private_delivery(&self, task: &store::Task) -> Result<bool> {
        let Some(dispatch) = task
            .dispatch_message
            .as_deref()
            .filter(|dispatch| !dispatch.is_empty())
        else {
            return Ok(false);
        };
        let Some((origin, _)) = self.private_origin(task)? else {
            return Ok(false);
        };
        if origin.dispatch != dispatch {
            return Ok(false);
        }
        let id = origin.delivery_id();
        self.open_private_ledger(origin)?;
        crate::delivery::private::set_state(
            &self.state_dir,
            &id,
            crate::delivery::private::PrivateState::Failed,
        )?;
        Ok(true)
    }

    /// Recover completed dispatches that predate the ledger's last write
    /// for one private task, from durable native evidence only. The current
    /// task pointer is never used: each dispatch message must be matched to
    /// exactly one `task_dispatched` and one `task_reported` event with the
    /// same task/job/worker/revision/message, and the dispatch event must
    /// name the project bound at dispatch. Anything missing, duplicated,
    /// malformed or disagreeing is reported as unavailable, never recovered.
    ///
    /// `only_dispatch` scopes the pass to one exact kickoff: the daemon's
    /// completion paths use it to record a just-committed result whose
    /// task pointer has since moved to another revision, through the same
    /// canonical evidence as a full recovery scan.
    fn record_historical_private_results(
        &self,
        job_id: &str,
        task: &store::Task,
        unavailable: &mut Vec<Value>,
        only_dispatch: Option<&str>,
    ) -> Result<usize> {
        let events = self.store.task_history_events(job_id, &task.id)?;
        let messages = self.store.task_dispatch_messages(&task.id)?;
        if messages.is_empty() && events.is_empty() {
            return Ok(0);
        }
        let Some(ctx) = self.private_job_context(job_id)? else {
            return Ok(0);
        };
        let current = task.dispatch_message.as_deref().filter(|dispatch| {
            task.state != "draft" && task.revision > 0 && !dispatch.trim().is_empty()
        });
        // Completeness signals of the full scan: an unreadable event or a
        // dispatch event with no durable message is reported, never
        // silently skipped.
        let mut dispatch_events: BTreeSet<&str> = BTreeSet::new();
        for event in &events {
            match event
                .message
                .as_deref()
                .filter(|message| !message.is_empty())
            {
                Some(message) if event.kind == "task_dispatched" => {
                    dispatch_events.insert(message);
                }
                Some(_) => {}
                None => {
                    if only_dispatch.is_none() {
                        unavailable.push(json!({
                            "job": job_id, "task": task.id,
                            "error": format!("an unreadable {} event cannot prove a dispatch", event.kind),
                        }));
                    }
                }
            }
        }
        let durable: BTreeSet<&str> = messages.iter().map(|message| message.id.as_str()).collect();
        match only_dispatch {
            Some(dispatch) => {
                if !durable.contains(dispatch) {
                    unavailable.push(json!({
                        "job": job_id, "task": task.id, "message": dispatch,
                        "error": "completed dispatch message is missing from the store",
                    }));
                }
            }
            None => {
                for message in &dispatch_events {
                    if !durable.contains(message) {
                        unavailable.push(json!({
                            "job": job_id, "task": task.id, "message": message,
                            "error": "task_dispatched event has no durable job_dispatch message",
                        }));
                    }
                }
            }
        }
        let mut reconciled = 0;
        for message in &messages {
            if only_dispatch.is_some_and(|dispatch| dispatch != message.id.as_str()) {
                continue;
            }
            if message.state != "completed"
                || (only_dispatch.is_none() && current == Some(message.id.as_str()))
            {
                continue;
            }
            let binding = match self.private_dispatch_binding(&ctx, &task.id, &message.id, &events)
            {
                Ok(binding) => binding,
                Err(error) => {
                    unavailable.push(json!({
                        "job": job_id, "task": task.id, "message": message.id,
                        "error": error.to_string(),
                    }));
                    continue;
                }
            };
            let proof = match self.private_dispatch_proof(&ctx, &task.id, binding, &events) {
                Ok(proof) => proof,
                Err(error) => {
                    unavailable.push(json!({
                        "job": job_id, "task": task.id, "message": message.id,
                        "error": error.to_string(),
                    }));
                    continue;
                }
            };
            let origin = match crate::delivery::private::VerifiedJobOrigin::for_dispatch(
                &ctx.job,
                &task.id,
                proof.binding.revision,
                &message.id,
                &ctx.issue,
                &ctx.request,
                &proof.binding.project,
            ) {
                Ok(origin) => origin,
                Err(error) => {
                    unavailable.push(json!({
                        "job": job_id, "task": task.id, "message": message.id,
                        "error": error.to_string(),
                    }));
                    continue;
                }
            };
            let id = origin.delivery_id();
            self.open_private_ledger(origin)?;
            let result = crate::delivery::private::PrivateResult {
                message: message.id.clone(),
                revision: proof.binding.revision,
                digest: crate::delivery::private::result_digest(
                    &message.id,
                    proof.sha.as_deref(),
                    &proof.body,
                ),
                sha: proof.sha,
                body: proof.body,
                at: crate::issue::time::now_epoch(),
            };
            crate::delivery::private::append_result(&self.state_dir, &id, result)?;
            reconciled += 1;
        }
        Ok(reconciled)
    }

    /// Record the private delivery of a kickoff whose durable completion
    /// was just committed — by `finish`/`finish_running`, `message
    /// report`, an operator reconcile or the cloud runner. The durable
    /// message is read back, so a stale pre-completion row decides
    /// nothing. No shared effect, no task replay, never ordinary work: a
    /// non-member job and an incomplete message answer `Ok(false)`.
    ///
    /// The result is bound through [`Self::recorded_report_sha`], the one
    /// canonical SHA source, so the digest a later verdict, reopen or
    /// recovery computes is identical. An unprovable message is an error
    /// for the caller to report as unavailable; the committed completion
    /// itself stands and the same native rows stay recoverable.
    pub(super) fn record_private_completion(&self, message_id: &str) -> Result<bool> {
        let Some(message) = self.store.message(message_id)? else {
            return Ok(false);
        };
        if message.source != "job_dispatch" || message.state != "completed" {
            return Ok(false);
        }
        let Some(task_id) = message.task_id.as_deref() else {
            return Ok(false);
        };
        let task = match self.store.task(task_id) {
            Ok(task) => task,
            Err(Error::Rejected(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        if !self.store.assist_job_has_request(&task.job_id)? {
            return Ok(false);
        }
        if task.dispatch_message.as_deref() == Some(message.id.as_str())
            && task.state != "draft"
            && task.revision > 0
        {
            return self.record_private_result(&task);
        }
        // A reopen and a fresh kickoff moved the task pointer while this
        // older message was still running: its own durable dispatch/report
        // pair proves the delivery, exactly as historical recovery does.
        let mut unavailable = Vec::new();
        let recorded = self.record_historical_private_results(
            &task.job_id,
            &task,
            &mut unavailable,
            Some(message.id.as_str()),
        )?;
        if let Some(row) = unavailable.into_iter().next() {
            let reason = row
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("native evidence is unavailable");
            return Err(Error::rejected(format!(
                "private delivery: dispatch {message_id}: {reason}"
            )));
        }
        Ok(recorded > 0)
    }

    /// The RPC-visible settlement of a just-committed completion. A ledger
    /// failure is logged and reported as `unavailable`, never hidden and
    /// never turned into a failure of the already-committed completion.
    pub(super) fn settle_private_completion(&self, message_id: &str) -> Value {
        match self.record_private_completion(message_id) {
            Ok(true) => json!({"state": "recorded"}),
            Ok(false) => json!({"state": "not_private"}),
            Err(error) => {
                tracing::error!("private delivery for message {message_id}: {error}");
                json!({"state": "unavailable", "error": error.to_string()})
            }
        }
    }

    /// Retry private-ledger recovery strictly from native rows. Opens a
    /// missing entry and appends the exact completed result; never reruns
    /// a task and never writes a shared effect. `unavailable` lists the
    /// identities whose ledger could not be written, so a failure is
    /// reported rather than hidden.
    pub(super) fn reconcile_private_deliveries(&self, only_job: Option<&str>) -> Result<Value> {
        let summary = self.store.job_task_summary(Some(false))?;
        let mut reconciled = 0usize;
        let mut unavailable = Vec::new();
        for (job_id, tasks) in &summary.lists {
            if only_job.is_some_and(|job| job != job_id.as_str()) {
                continue;
            }
            match self.store.assist_job_has_request(job_id) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    unavailable.push(json!({"job": job_id, "error": error.to_string()}));
                    continue;
                }
            }
            for task in tasks {
                match self.record_private_result(task) {
                    Ok(true) => reconciled += 1,
                    Ok(false) => {}
                    Err(error) => unavailable.push(json!({
                        "job": job_id,
                        "task": task.id,
                        "error": error.to_string(),
                    })),
                }
                match self.record_historical_private_results(job_id, task, &mut unavailable, None) {
                    Ok(count) => reconciled += count,
                    Err(error) => unavailable.push(json!({
                        "job": job_id,
                        "task": task.id,
                        "error": error.to_string(),
                    })),
                }
            }
        }
        let rows: Vec<Value> = crate::delivery::private::load(&self.state_dir)?
            .values()
            .filter(|delivery| only_job.is_none_or(|job| delivery.origin.job == job))
            .map(|delivery| delivery.to_json())
            .collect();
        Ok(json!({
            "reconciled": reconciled,
            "unavailable": unavailable,
            "deliveries": rows,
        }))
    }

    /// `job dispatch` — bookkeeping plus the kickoff enqueue in one
    /// store transaction. Does not bypass the ready gate: `--ready` is
    /// claimed client-side exactly like `send --ready`.
    pub(super) fn rpc_task_dispatch(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let to = optional_str(params, "to")
            .map(|a| self.resolve_alias(a))
            .transpose()?;
        let by = required_str(params, "by")?;
        let task_id = required_str(params, "task")?;
        let _grant = self
            .unattended_lock
            .read()
            .unwrap_or_else(|e| e.into_inner());
        self.plan_gate_task(task_id)?;
        // CAD-1846: capture the exact native task/job/worker and the proposed
        // kickoff revision/id before any source read. An accepted live
        // kickoff keeps that identity; only a fresh kickoff prepares context.
        let plan = self.store.plan_task_dispatch(
            task_id,
            to.as_deref(),
            optional_str(params, "message"),
        )?;
        // The planned worker is the pre-transaction backend choice; the
        // fenced admission re-reads its identity, so a registry change
        // between here and the write refuses instead of retargeting.
        crate::adapter::pi::refuse_confined_devin_agent(&plan.worker)?;
        if !plan.worker.enabled {
            return Err(Error::rejected(format!(
                "Agent '{}' is disabled — resume it before dispatching",
                plan.worker.alias
            )));
        }
        let cloud_dispatch = plan.worker.endpoint_kind == "sandbox";
        // Read-only AOS probe before the dispatch transaction. A denied
        // company leaves the task revision, job and kickoff untouched.
        if cloud_dispatch {
            // A member-private job must never reach the shared cloud lane.
            // Refuse before the entitlement probe so no network call and
            // no native dispatch effect can happen for it.
            crate::issue::cloud::refuse_private_job(&self.store, &plan.job, &plan.worker.alias)?;
            crate::issue::cloud::probe_entitlement()?;
        }
        let stall_secs = optional_i64(params, "stall_secs");
        if stall_secs.is_some_and(|s| s < 0) {
            return Err(Error::rejected("stall_secs must be non-negative"));
        }
        let no_lessons = match params.get("no_lessons") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err(Error::rejected("no_lessons must be a boolean")),
        };
        // A live accepted kickoff is admitted with its original identity and
        // selection: never recapture issue/spec sources for that retry.
        let delivery = if plan.duplicate {
            None
        } else {
            // CAD-622: a fresh kickoff is a new authoring act — the app-team
            // guard runs before any context capture or queue write. A
            // duplicate is the accepted kickoff's own retry, not new work.
            self.native_app_guard(&plan, optional_str(params, "by"))?;
            Some(self.prepare_job_delivery(&plan, no_lessons)?)
        };
        // CAD-1380 deferred commit: an assist-bound job's dispatch re-proves
        // the member's live authority at consume time — the approval gate
        // alone is not the commit. This final proof runs after every blocking
        // read above (source capture included), immediately before admission,
        // so a long context capture cannot stale the authority it proved.
        // The stored `assist_requests` row's `credential_id`/`authority_rev`
        // name the provenance the consume re-proves (never the member's
        // latest mint), while the assistant the pull binds is the CURRENT
        // live binding's — a revoked binding fails, a re-minted one names the
        // current principal. The final assignee — the request's `to` or the
        // task's stored `assignee` — is checked after, never silently skipped.
        //
        // A store error on the provenance read propagates — the guard never
        // silently skips the re-prove on an unreadable table. And a job in the
        // `assist-` id family with NO provenance row is fail-closed too: its
        // binding was deleted or corrupted, never a silent drop to an ordinary
        // dispatch (a missing row cannot be re-proven, so the member's
        // recorded authority is unverifiable). The fenced admission below
        // re-reads the captured provenance under the write lock.
        let mut expect: Option<crate::store::AssistDispatchExpect> = None;
        let mut private_capture = None;
        match self.store.assist_request_for_job(&plan.job.id)? {
            Some(assist) => {
                let binding = self
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
                    state: binding.state.clone(),
                    credential_id: assist.credential_id.clone().unwrap_or_default(),
                };
                // Capture the exact issue location before the live authority
                // pull. It is re-resolved under the tracker writer lock before
                // the dispatch transaction; a move cannot relabel the kickoff.
                let issue = plan.job.issue_id.as_deref().ok_or_else(|| {
                    Error::rejected("task dispatch refused: the private job has no issue")
                })?;
                let pm = self.pm()?;
                let (project, issue_dir) = crate::issue::write::issue_dir(&pm, issue)?;
                self.member_authority_live(
                    // No member envelope survives to consume time — the
                    // deferred-commit bearer is the enrolled `hosted-board`
                    // service credential (`assist.revalidate`); absent one,
                    // the empty envelope produces a bearer the platform
                    // rejects — fail closed. `operation` is the daemon's
                    // internal deferred-commit name — admitted by the strict
                    // revalidate schema, absent from the browser-callable
                    // verb table (`/call` cannot mint it).
                    "",
                    &member,
                    crate::daemon::members_rpc::MemberReprove {
                        operation: "job_dispatch",
                        scope: "assist.jobs",
                        job_id: Some(&plan.job.id),
                        // The queued effect's own recorded provenance — the
                        // `assist_requests.credential_id` it committed under.
                        // The platform re-proves THIS credential live, never
                        // the binding's latest, so a stale dispatch denied
                        // after revocation does NOT ride a renewal.
                        op_credential_id: assist.credential_id.as_deref(),
                    },
                    "task dispatch",
                )?;
                expect = Some(crate::store::AssistDispatchExpect {
                    org: assist.org.clone(),
                    sub: assist.sub.clone(),
                    assistant_id: binding.assistant_id.clone(),
                    alias: binding.alias.clone(),
                    credential_id: assist.credential_id.clone(),
                    job: plan.job.id.clone(),
                });
                // The member-assistant PM never dispatches its own bound
                // job — that would be self-approval. Check the exact planned
                // worker — the request's `to` OR the task's stored assignee.
                if plan.worker.alias == binding.alias {
                    return Err(Error::rejected(format!(
                        "task dispatch refused: the assist-bound job cannot be \
                     dispatched to its own assistant '{}' — that is \
                     self-approval (CAD-1380)",
                        binding.alias
                    )));
                }
                private_capture = Some((
                    pm,
                    issue_dir,
                    project.key,
                    member,
                    assist.credential_id.clone(),
                ));
            }
            // The `assist-<digest>` id family is minted ONLY by
            // `member_job_request`; a job in that family with no provenance
            // row cannot be re-proven — refuse rather than dispatch as though
            // it were an ordinary job.
            None if plan.job.id.starts_with("assist-") => {
                return Err(Error::rejected(format!(
                    "task dispatch refused: job '{}' is assist-bound but has no \
                     `assist_requests` provenance — the member's authority cannot be \
                     re-proven at commit (CAD-1380)",
                    plan.job.id
                )));
            }
            None => {}
        }
        let (task, message, duplicate, behind_dead) =
            if let Some((pm, issue_dir, project, member, credential)) = private_capture {
                let policy = issue_dir
                    .parent()
                    .ok_or_else(|| {
                        Error::rejected("task dispatch refused: issue has no project directory")
                    })?
                    .join("project.yaml");
                // The scoped tracker lock serializes supported issue moves and
                // project policy writes through the database commit.
                let _tracker = pm.lock_for_paths(&[issue_dir.clone(), policy])?;
                let issue = plan.job.issue_id.as_deref().ok_or_else(|| {
                    Error::rejected("task dispatch refused: the private job has no issue")
                })?;
                let (current, current_dir) = crate::issue::write::issue_dir(&pm, issue)?;
                if current_dir != issue_dir || current.key != project {
                    return Err(Error::rejected(
                        "task dispatch refused: the issue moved while dispatch was being prepared",
                    ));
                }
                let actor = crate::project_policy::policy_user_actor(&member.sub);
                let caller = crate::project_policy::caller_of(&actor).ok_or_else(|| {
                    Error::rejected("task dispatch refused: invalid member identity")
                })?;
                if !current.allows(caller) {
                    return Err(Error::rejected(
                        "task dispatch refused: the member can no longer read the issue's project",
                    ));
                }
                // The first pull can have waited for the tracker lock; a
                // second live proof is the one immediately before the effect.
                self.member_authority_live(
                    "",
                    &member,
                    crate::daemon::members_rpc::MemberReprove {
                        operation: "job_dispatch",
                        scope: "assist.jobs",
                        job_id: Some(&plan.job.id),
                        op_credential_id: credential.as_deref(),
                    },
                    "task dispatch",
                )?;
                self.store.dispatch_task_planned(
                    &plan,
                    delivery,
                    by,
                    expect.as_ref(),
                    stall_secs,
                    Some(&project),
                )?
            } else {
                self.store.dispatch_task_planned(
                    &plan,
                    delivery,
                    by,
                    expect.as_ref(),
                    stall_secs,
                    None,
                )?
            };
        // CAD-2215: an assist-bound job's delivery is private state. The
        // ledger write is idempotent and never touches `delivery.json`.
        // A failure does not undo the committed dispatch; it is reported
        // truthfully and can be recovered from the same native rows.
        let private_delivery = match self.open_private_delivery(&task) {
            Ok(Some(())) => json!({"state": "opened"}),
            Ok(None) => json!({"state": "not_private"}),
            Err(error) => {
                tracing::error!("private delivery for task {}: {error}", task.id);
                json!({"state": "unavailable", "error": error.to_string()})
            }
        };
        if cloud_dispatch {
            let shared = Arc::clone(self);
            let task_id = task.id.clone();
            let message_id = message.clone();
            std::thread::spawn(move || {
                if let Err(error) = shared.run_cloud_dispatch(&task_id, &message_id) {
                    eprintln!("cloud sandbox dispatch {task_id} failed: {error}");
                }
            });
        }
        if let Some(assignee) = &task.assignee {
            self.notify_agent(assignee);
        }
        let message_state = self.store.message(&message)?.map(|m| m.state);
        self.wake();
        Ok(json!({"task": task.to_json(), "message": message,
                  "message_state": message_state,
                  "duplicate": duplicate, "queued_behind_dead": behind_dead,
                  "private_delivery": private_delivery}))
    }

    /// CAD-1846: the complete issue-linked job kickoff selected from the
    /// issue's own server-derived project and pinned context, or the legacy
    /// daemon-templated kickoff paired with an honest `Disabled` selection
    /// when the job has no issue binding or its project is not opted in to
    /// context delivery.
    ///
    /// Runs entirely outside the dispatch write transaction: callers prepare
    /// the delivery first and hand it to `dispatch_task_planned`, which writes
    /// the message, the pin/disabled marker and the task transition
    /// atomically. The caller has already run `plan_gate_task` for this task;
    /// this helper adds no broader parent-policy gate. An accepted live retry
    /// must never call this — its original selection already committed, and
    /// recapturing moved sources would retarget it.
    pub(super) fn prepare_job_delivery(
        &self,
        plan: &crate::store::DispatchPlan,
        no_lessons: bool,
    ) -> Result<(String, store::IssueSelection)> {
        let legacy = |reason: String| -> Result<(String, store::IssueSelection)> {
            Ok((
                store::job_kickoff(
                    &plan.job,
                    &plan.task,
                    plan.revision,
                    &plan.message_id,
                    &plan.worker.provider,
                    &plan.worker.endpoint_kind,
                    false,
                )?,
                store::IssueSelection::Disabled {
                    task: plan.task.id.clone(),
                    reason,
                },
            ))
        };
        let Some(issue_id) = plan.job.issue_id.as_deref() else {
            return legacy("the job is not bound to a tracker issue".to_string());
        };
        let pm = self.pm()?;
        // Capture the issue snapshot without re-running the parent policy
        // gate: `plan_gate_task` already applied exactly that gate for every
        // native caller. A named id no project holds keeps its legacy kickoff
        // (`gate_id` documents that jobs need not name a tracker issue); an id
        // a project holds but cannot be read/parsed fails closed.
        let Some(gated) = crate::issue::dispatch::capture_issue(&pm, issue_id)? else {
            self.refuse_known_opted_in_unheld(&pm, plan)?;
            return legacy(
                "no native issue context binding: the job's tracker issue is not \
                 held by a project"
                    .to_string(),
            );
        };
        let target = crate::issue::dispatch::ContextTarget {
            team_role: plan.worker.team_role.as_deref(),
            provider: &plan.worker.provider,
            endpoint_kind: &plan.worker.endpoint_kind,
        };
        // CAD-1846: the native scope proof, run only after the project is
        // confirmed opted in and its endpoint can receive context, and before
        // any spec byte is read. The job's declared repository must be exactly
        // the checkout of this issue's single verified open lane, and the
        // task's worktree must be that same lane — not merely another
        // worktree in the same repository. Canonicalization failures fail
        // closed rather than falling back to a textual comparison.
        let scope = |project: &crate::issue::project::Project,
                     front: &crate::issue::model::Front|
         -> Result<()> {
            let lane = self.dispatch_lane("job dispatch", project, front)?;
            let lane_root = crate::worktree::main_root(&lane).map_err(|error| {
                crate::context_pack::Refusal::ScopeConflict {
                    detail: format!(
                        "issue {}'s lane {} has no proven repository: {error}",
                        front.id,
                        lane.display()
                    ),
                }
                .error()
            })?;
            let repo = plan.job.repo.as_deref().ok_or_else(|| {
                crate::context_pack::Refusal::ScopeConflict {
                    detail: format!("issue-linked job '{}' names no repository", plan.job.id),
                }
                .error()
            })?;
            let repo = std::path::Path::new(repo).canonicalize().map_err(|error| {
                crate::context_pack::Refusal::ScopeConflict {
                    detail: format!(
                        "job '{}' repository {repo} cannot be resolved: {error}",
                        plan.job.id
                    ),
                }
                .error()
            })?;
            if repo != lane_root {
                return Err(crate::context_pack::Refusal::ScopeConflict {
                    detail: format!(
                        "job '{}' names repository {}, but issue {}'s lane {} is in {}",
                        plan.job.id,
                        repo.display(),
                        front.id,
                        lane.display(),
                        lane_root.display()
                    ),
                }
                .error());
            }
            let worktree = plan.task.worktree.as_deref().ok_or_else(|| {
                crate::context_pack::Refusal::ScopeConflict {
                    detail: format!("issue-linked task '{}' binds no worktree", plan.task.id),
                }
                .error()
            })?;
            let task_lane = crate::worktree::layout::worktree_dir(&lane_root, worktree);
            let task_lane = task_lane.canonicalize().map_err(|error| {
                crate::context_pack::Refusal::ScopeConflict {
                    detail: format!(
                        "task '{}' worktree {} cannot be resolved: {error}",
                        plan.task.id,
                        task_lane.display()
                    ),
                }
                .error()
            })?;
            if task_lane != lane {
                return Err(crate::context_pack::Refusal::ScopeConflict {
                    detail: format!(
                        "task '{}' binds worktree {}, not issue {}'s verified lane {}",
                        plan.task.id,
                        task_lane.display(),
                        front.id,
                        lane.display()
                    ),
                }
                .error());
            }
            Ok(())
        };
        let prepared = crate::issue::dispatch::job_context(
            &pm,
            gated,
            crate::issue::dispatch::JobContext {
                job: &plan.job,
                task: &plan.task,
                revision: plan.revision,
                message: &plan.message_id,
                target,
                admission_scope: Some(&scope),
            },
            no_lessons,
        )?;
        let Some(prepared) = prepared else {
            return legacy("the assigned project is not opted in to agent delivery".to_string());
        };
        Ok((
            prepared.body.into_owned(),
            store::IssueSelection::Pin(prepared.manifest.pin_payload(&plan.message_id)),
        ))
    }

    /// CAD-1846: a job whose tracker id no project holds is normally legacy
    /// (jobs need not name a tracker issue). But when the job's declared
    /// repository belongs to a registered project that explicitly opted in to
    /// agent delivery, the missing canonical issue context is a known
    /// requirement and must fail closed instead of being mislabeled as an
    /// opt-out. One bounded pin over the registered projects, only on this
    /// exceptional resolution path; no default or current-screen project is
    /// ever guessed.
    fn refuse_known_opted_in_unheld(
        &self,
        pm: &crate::issue::Pm,
        plan: &crate::store::DispatchPlan,
    ) -> Result<()> {
        let Some(repo) = plan.job.repo.as_deref() else {
            return Ok(());
        };
        let Ok(repo) = std::path::Path::new(repo).canonicalize() else {
            return Ok(());
        };
        let (role, _) = crate::context_pack::selection_role(plan.worker.team_role.as_deref());
        for project in crate::issue::project::list(&pm.dir)? {
            if !crate::issue::start::declared_repos(&project)
                .iter()
                .any(|root| root == &repo)
            {
                continue;
            }
            if crate::issue::context::pin(&project, Some(role)).state != "unconfigured" {
                return Err(crate::context_pack::Refusal::ScopeConflict {
                    detail: format!(
                        "job '{}' names issue '{}' that no project holds, while its \
                         repository {} is declared by opted-in project '{}'; the \
                         canonical issue context cannot be resolved",
                        plan.job.id,
                        plan.job.issue_id.as_deref().unwrap_or_default(),
                        repo.display(),
                        project.key
                    ),
                }
                .error());
            }
        }
        Ok(())
    }

    /// Consume a sandbox agent's dispatched kickoff. The normal dispatch
    /// transaction already set the task `dispatched` and queued `message`;
    /// claim the queued delivery before any external side effect, then drive
    /// the remote job and call the same finish/report path. A cancelled or
    /// already-claimed delivery is left alone. `has_actor=false` means no
    /// actor exists.
    pub(super) fn run_cloud_dispatch(&self, task_id: &str, message_id: &str) -> Result<()> {
        fn fail(
            store: &crate::store::Store,
            message: &crate::store::Message,
            error: Error,
        ) -> Error {
            if store.cloud_create_attempted(&message.id).unwrap_or(false) {
                let _ = store.requeue_cloud_claim(&message.id, &format!("cloud:{}", message.id));
                return Error::Structured(crate::error::Structured {
                    kind: "rejected",
                    code: crate::issue::cloud::UNCERTAIN.to_string(),
                    message: format!(
                        "cloud sandbox recovery preflight failed after a prior create \
                         attempt: {error}"
                    ),
                    revision: None,
                });
            }
            crate::issue::cloud::fail_message(store, message, &error);
            error
        }
        let message = self.store.message(message_id)?.ok_or_else(|| {
            Error::rejected(format!("cloud dispatch message '{message_id}' is gone"))
        })?;
        // CAD-2215: a legacy queued private kickoff is refused BEFORE any
        // claim or recovery. A prior remote create stays unknown (the
        // guard settles that), never a fabricated interruption or a
        // second writer.
        crate::issue::cloud::refuse_dispatched_kickoff(&self.store, &message)?;
        let turn = format!("cloud:{}", message.id);
        let claimed = if message.state == "unknown" {
            self.store.claim_cloud_recovery(&message.id, &turn)?
        } else {
            self.store.mark_running_queued(&message.id, &turn)?
        };
        if !claimed {
            return Ok(());
        }
        let task = self
            .store
            .task(task_id)
            .map_err(|error| fail(&self.store, &message, error))?;
        let job = self
            .store
            .job(&task.job_id)
            .map_err(|error| fail(&self.store, &message, error))?;
        let alias = task.assignee.as_deref().unwrap_or(message.alias.as_str());
        let agent = self
            .store
            .agent(alias)
            .map_err(|error| fail(&self.store, &message, error))?;
        if agent.provider != "sandbox" || agent.endpoint_kind != "sandbox" {
            let error = Error::rejected(format!(
                "cloud kickoff recipient '{alias}' is no longer a sandbox agent"
            ));
            return Err(fail(&self.store, &message, error));
        }
        let _ = crate::issue::cloud::hosted_company(|key| std::env::var(key).ok())
            .map_err(|error| fail(&self.store, &message, error))?;
        let pm = self
            .pm()
            .map_err(|error| fail(&self.store, &message, error))?;
        loop {
            let outcome = crate::issue::cloud::run_dispatch(
                &pm,
                &self.store,
                &task,
                &job,
                &message,
                &agent,
                &|| {
                    self.store
                        .cloud_stop_requested(&message.id)
                        .unwrap_or(false)
                },
            );
            if let Err(error) = &outcome {
                if error.code() == Some(crate::issue::cloud::UNCERTAIN) {
                    let _ = self
                        .store
                        .requeue_cloud_claim(&message.id, &format!("cloud:{}", message.id));
                    // Re-enter the same kickoff/remote identity instead of
                    // leaving a queued claim without a consumer.
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    if !self
                        .store
                        .mark_running_queued(&message.id, &format!("cloud:{}", message.id))?
                    {
                        return Ok(());
                    }
                    continue;
                }
            }
            // CAD-2215: the cloud runner settled the kickoff itself, so
            // its durable completion is the private ledger's trigger.
            if outcome.is_ok() {
                let _ = self.settle_private_completion(message_id);
            }
            return outcome;
        }
    }

    /// `job verdict` — the reviewer is the verified caller (CAD-372),
    /// never a request field: the agent whose pane or enrolled managed
    /// endpoint the connection descends from, or `operator` on positive
    /// operator proof ([`Self::agent_caller`]). `reviewer`, `pane`, `by`
    /// and the other identity fields are refused, not read. The task's
    /// assignee — and the agent that reported the judged revision, its
    /// author — can never judge it (the store re-checks the assignee in
    /// the verdict transaction). The store binds the verdict to
    /// `head_sha` + current revision.
    pub(super) fn rpc_task_verdict(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let task_id = required_str(params, "task")?;
        let sha = required_str(params, "sha")?;
        let verdict = required_str(params, "verdict")?;
        reject_identity_fields(params, "job verdict")?;
        let caller = self.agent_caller(peer_pid, "job verdict")?;
        // The verdict row names the verified caller; `pane` records that
        // it was an agent's (the event's `pane` field, as before).
        let reviewer = caller.audit().0.to_string();
        let pane = match &caller {
            AgentCaller::Agent(alias) => Some(alias.as_str()),
            AgentCaller::Operator => None,
        };
        if let AgentCaller::Agent(alias) = &caller {
            let task = self.read_task_for(&caller, task_id)?;
            let author = match &task.dispatch_message {
                Some(id) => self.store.message(id)?.map(|m| m.alias),
                None => None,
            };
            let role = if task.assignee.as_deref() == Some(alias) {
                Some("assignee")
            } else if author.as_deref() == Some(alias) {
                Some("author of the reported revision")
            } else {
                None
            };
            if let Some(role) = role {
                return Err(Error::rejected(format!(
                    "job verdict refused: this connection is agent '{alias}', the \
                     task's {role} — a worker cannot verdict its own work; the \
                     reviewer is the verified caller (reviewer independence, CAD-372)"
                )));
            }
        }
        let verify = params
            .get("verify")
            .filter(|v| !v.is_null())
            .map(|v| v.to_string());
        let (task, v) = self.store.record_verdict(
            task_id,
            sha,
            verdict,
            &reviewer,
            pane,
            optional_str(params, "evidence"),
            optional_str(params, "message"),
            optional_i64(params, "revision"),
            verify.as_deref(),
        )?;
        let job = self.store.job(&task.job_id)?;
        // CAD-2215: the task's durable result — when it has one — is
        // bound to the private ledger here. Nothing shared is written.
        let private_delivery = match self.record_private_result(&task) {
            Ok(true) => json!({"state": "recorded"}),
            Ok(false) => json!({"state": "not_recorded"}),
            Err(error) => {
                tracing::error!("private delivery for task {}: {error}", task.id);
                json!({"state": "unavailable", "error": error.to_string()})
            }
        };
        self.notify_agent(&job.pm_alias);
        self.wake();
        Ok(json!({"task": task.to_json(), "verdict": v.to_json(),
                  "private_delivery": private_delivery}))
    }

    pub(super) fn rpc_task_accept(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = required_str(params, "by")?;
        let task = self.store.accept_task(
            required_str(params, "task")?,
            optional_str(params, "merged_sha"),
            by,
        )?;
        let private_delivery = match self.record_private_result(&task) {
            Ok(true) => json!({"state": "recorded"}),
            Ok(false) => json!({"state": "not_recorded"}),
            Err(error) => {
                tracing::error!("private delivery for task {}: {error}", task.id);
                json!({"state": "unavailable", "error": error.to_string()})
            }
        };
        let job = self.store.job(&task.job_id)?;
        self.notify_agent(&job.pm_alias);
        self.wake();
        Ok(json!({"task": task.to_json(), "private_delivery": private_delivery}))
    }

    /// `job task sha` — the repair path for a `review` task whose
    /// kickoff reported no SHA (A3): the PM/operator records it
    /// explicitly; never inferred.
    pub(super) fn rpc_task_sha(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = required_str(params, "by")?;
        let task = self.store.set_task_sha(
            required_str(params, "task")?,
            required_str(params, "sha")?,
            by,
        )?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    pub(super) fn rpc_task_fail(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = required_str(params, "by")?;
        let task = self.store.fail_task(
            required_str(params, "task")?,
            required_str(params, "reason")?,
            by,
        )?;
        let private_delivery = match self.fail_private_delivery(&task) {
            Ok(true) => json!({"state": "failed"}),
            Ok(false) => json!({"state": "not_private"}),
            Err(error) => {
                tracing::error!("private delivery for task {}: {error}", task.id);
                json!({"state": "unavailable", "error": error.to_string()})
            }
        };
        self.wake();
        Ok(json!({"task": task.to_json(), "private_delivery": private_delivery}))
    }

    /// `job task reopen` — the proven operator or the job's own PM
    /// (skills/cadence/SKILL.md "Job work": PMs run it). Authority is the
    /// connection's (CAD-373, [`Self::agent_caller`]): an agent may
    /// reopen only a task of a job whose recorded `pm` is that agent,
    /// and never one assigned to itself — the assignee, a peer and
    /// another group's PM are refused. No request field (`pane`, `by`,
    /// …) decides who is asking; the record names the verified caller.
    pub(super) fn rpc_task_reopen(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        // No `reject_identity_fields` here: the OnJob caller rule stamps
        // `by` for admitted callers, and the gate already refused any
        // agent-supplied identity field upstream.
        let task_id = required_str(params, "task")?;
        let caller = self.agent_caller(peer_pid, "job task reopen")?;
        if let AgentCaller::Agent(alias) = &caller {
            // The caller rule already bound the caller to the job's PM
            // (CAD-422); the assignee never reopens its own task.
            let task = self.store.task(task_id)?;
            if task.assignee.as_deref() == Some(alias.as_str()) {
                return Err(Error::rejected(format!(
                    "job task reopen refused: agent '{alias}' is the task's \
                     assignee — a task is reopened only by the operator or its \
                     job's own PM (caller rule, CAD-373)"
                )));
            }
        }
        let task = self.store.reopen_task(task_id, caller.audit().0)?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    pub(super) fn rpc_task_cancel(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = required_str(params, "by")?;
        let task = self.store.cancel_task(required_str(params, "task")?, by)?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    pub(super) fn rpc_job_cancel(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = required_str(params, "by")?;
        let job = self.store.cancel_job(required_str(params, "job")?, by)?;
        self.wake();
        Ok(json!({"job": job.to_json()}))
    }

    pub(super) fn rpc_job_close(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = required_str(params, "by")?;
        let job = self.store.close_job(required_str(params, "job")?, by)?;
        self.wake();
        Ok(json!({"job": job.to_json()}))
    }
}
