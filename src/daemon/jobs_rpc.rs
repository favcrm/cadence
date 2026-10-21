//! CAD-534: `cadence daemon` jobs RPC handlers — moved verbatim from src/daemon.rs.

use super::members_rpc::MemberCaller;
use super::*;

impl Shared {
    // ---- Jobs: the work axis (docs/JOBS.md) ----

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
        }
        let spec = required_str(params, "spec")?;
        let spec_sha256 = required_str(params, "spec_sha256")?;
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
        for job in jobs {
            // CAD-1380: a member-private job is withheld from every caller
            // except the operator, its owner, an authorized actor, or a
            // subscriber, before it is serialized.
            if self.member_job_row_withheld(&job.id, peer_pid)? {
                continue;
            }
            let mut j = job.to_json();
            j["tasks"] = json!(summary.counts.remove(&job.id).unwrap_or_default());
            if detail {
                j["task_list"] = json!(summary
                    .lists
                    .remove(&job.id)
                    .unwrap_or_default()
                    .iter()
                    .map(|t| json!({"id": t.id, "state": t.state, "title": t.title}))
                    .collect::<Vec<_>>());
            }
            out.push(j);
        }
        Ok(json!({"jobs": out}))
    }

    /// `job show` — job + tasks with live kickoff state, latest verdict
    /// and lazily-computed drift. No startup reconciliation runs: the
    /// read side flags a task whose kickoff ended without completing,
    /// a dead assignee, a missing SHA, or a spec that drifted since
    /// `job new` hashed it.
    pub(super) fn rpc_job_show(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "job")?;
        let job = self.store.job(id)?;
        let mut jj = job.to_json();
        if let Some(sha) = &job.spec_sha256 {
            match sha256_file(std::path::Path::new(&job.spec_path)) {
                Some(now) if now != *sha => {
                    jj["attention"] = json!(
                        "spec changed since `job new` — \
                        re-create the job if the drift is real work"
                    );
                }
                None => {
                    jj["attention"] = json!(format!("spec {} is unreadable", job.spec_path));
                }
                _ => {}
            }
        }
        let mut tasks = Vec::new();
        for task in self.store.tasks_for_job(id)? {
            tasks.push(self.task_json(&task)?);
        }
        jj["tasks"] = json!(tasks);
        Ok(json!({"job": jj}))
    }

    /// One task rendered for show/list: the row plus live kickoff
    /// state, drift flags and the latest verdict.
    fn task_json(self: &Arc<Self>, task: &store::Task) -> Result<Value> {
        let mut j = task.to_json();
        if let Some(mid) = &task.dispatch_message {
            match self.store.message(mid)? {
                Some(m) => {
                    j["kickoff"] = json!({"id": m.id, "state": m.state, "turn_id": m.turn_id});
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
        if task.state == "review" && task.head_sha.is_none() {
            j["attention"] = json!(format!(
                "kickoff reported no SHA — `cadence job task sha {} <sha>` \
                 records it before a verdict can land",
                task.id
            ));
        }
        let verdicts = self.store.verdicts_for_task(&task.id)?;
        if let Some(v) = store::current_verdict(task.revision, &verdicts) {
            j["latest_verdict"] = v.to_json();
        }
        Ok(j)
    }

    pub(super) fn rpc_task_show(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let task = self.store.task(required_str(params, "task")?)?;
        let mut out = self.task_json(&task)?;
        out["messages"] = json!(self
            .store
            .messages_for_task(&task.id)?
            .iter()
            .map(Message::to_json)
            .collect::<Vec<_>>());
        out["verdicts"] = json!(self
            .store
            .verdicts_for_task(&task.id)?
            .iter()
            .map(store::Verdict::to_json)
            .collect::<Vec<_>>());
        Ok(json!({"task": out}))
    }

    pub(super) fn rpc_job_events(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let job = self.store.job(required_str(params, "job")?)?;
        let after = params.get("after").and_then(Value::as_i64).unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Event cursor must be nonnegative"));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        // Same `tail` contract as agent_events — the job view's
        // default page is the newest too.
        if params.get("tail").and_then(Value::as_bool).unwrap_or(false) {
            let mut events = self.store.job_events_tail(&job.id, 51)?;
            let has_older = events.len() > 50;
            // The store returns oldest first; discard the extra oldest row.
            if has_older {
                events.remove(0);
            }
            return Ok(json!({
                "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
                "cursor": events.last().map(|e| e.seq).unwrap_or(0),
                "has_older": has_older,
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let events = self.store.job_events(&job.id, after, 200)?;
            if !events.is_empty() || self.closing.load(Ordering::SeqCst) {
                return Ok(json!({
                    "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
                    "cursor": events.last().map(|e| e.seq).unwrap_or(after),
                }));
            }
            if Instant::now() >= deadline {
                return Ok(json!({"events": [], "cursor": after}));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
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
        let pending = self.store.task(task_id)?;
        // CAD-1380 deferred commit: an assist-bound job's dispatch
        // re-proves the member's live authority at consume time — the
        // approval gate alone is not the commit. The stored
        // `assist_requests` row's `credential_id`/`authority_rev` name
        // the provenance the consume re-proves (never the member's
        // latest mint), while the assistant the pull binds is the
        // CURRENT live binding's — a revoked binding fails, a re-minted
        // one names the current principal. The final assignee — the
        // request's `to` or the task's stored `assignee` — is checked
        // after, never silently skipped.
        //
        // A store error on the provenance read propagates — the guard
        // never silently skips the re-prove on an unreadable table. And
        // a job in the `assist-` id family with NO provenance row is
        // fail-closed too: its binding was deleted or corrupted, never a
        // silent drop to an ordinary dispatch (a missing row cannot be
        // re-proven, so the member's recorded authority is unverifiable).
        // The local provenance this preflight proved; the fenced commit below
        // re-reads it under the write lock (CAD-1380 dispatch-local revocation).
        let mut expect: Option<crate::store::AssistDispatchExpect> = None;
        match self.store.assist_request_for_job(&pending.job_id)? {
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
                        job_id: Some(&pending.job_id),
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
                    job: pending.job_id.clone(),
                });
                // The member-assistant PM never dispatches its own bound
                // job — that would be self-approval. Check the FINAL
                // resolved assignee — the request's `to` OR the task's
                // stored `assignee` — never just the optional override.
                let final_assignee = to.as_deref().or(pending.assignee.as_deref());
                if final_assignee == Some(binding.alias.as_str()) {
                    return Err(Error::rejected(format!(
                        "task dispatch refused: the assist-bound job cannot be \
                     dispatched to its own assistant '{}' — that is \
                     self-approval (CAD-1380)",
                        binding.alias
                    )));
                }
            }
            // The `assist-<digest>` id family is minted ONLY by
            // `member_job_request`; a job in that family with no
            // provenance row cannot be re-proven — refuse rather than
            // dispatch as though it were an ordinary job.
            None if pending.job_id.starts_with("assist-") => {
                return Err(Error::rejected(format!(
                    "task dispatch refused: job '{}' is assist-bound but has no \
                     `assist_requests` provenance — the member's authority cannot be \
                     re-proven at commit (CAD-1380)",
                    pending.job_id
                )));
            }
            None => {}
        }
        let target_alias = to.as_deref().or(pending.assignee.as_deref());
        let target = target_alias
            .map(|alias| self.store.agent(alias))
            .transpose()?;
        if let Some(target) = &target {
            crate::adapter::pi::refuse_confined_devin_agent(target)?;
        }
        let cloud_dispatch = target
            .as_ref()
            .is_some_and(|agent| agent.endpoint_kind == "sandbox");
        // Read-only AOS probe before the dispatch transaction. A denied
        // company leaves the task revision, job and kickoff untouched.
        if cloud_dispatch {
            crate::issue::cloud::probe_entitlement()?;
        }
        let (task, message, duplicate, behind_dead) = match &expect {
            Some(expect) => self.store.dispatch_task_expecting_for(
                task_id,
                to.as_deref(),
                optional_str(params, "message"),
                by,
                expect,
                target_alias,
            )?,
            None => self.store.dispatch_task_for(
                task_id,
                to.as_deref(),
                optional_str(params, "message"),
                by,
                target_alias,
            )?,
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
        self.wake();
        Ok(json!({"task": task.to_json(), "message": message,
                  "duplicate": duplicate, "queued_behind_dead": behind_dead}))
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
            let task = self.store.task(task_id)?;
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
        self.notify_agent(&job.pm_alias);
        self.wake();
        Ok(json!({"task": task.to_json(), "verdict": v.to_json()}))
    }

    pub(super) fn rpc_task_accept(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = required_str(params, "by")?;
        let task = self.store.accept_task(
            required_str(params, "task")?,
            optional_str(params, "merged_sha"),
            by,
        )?;
        let job = self.store.job(&task.job_id)?;
        self.notify_agent(&job.pm_alias);
        self.wake();
        Ok(json!({"task": task.to_json()}))
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
        self.wake();
        Ok(json!({"task": task.to_json()}))
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
