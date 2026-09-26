//! CAD-534: `cadence daemon` jobs RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

impl Shared {
    // ---- Jobs: the work axis (docs/JOBS.md) ----

    /// `job new` — bookkeeping, not spawning. Requires a registered PM
    /// (an inbox alias is a legitimate PM — notifications drain through
    /// `cadence inbox`) and a readable spec the caller already hashed.
    pub(super) fn rpc_job_new(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let pm = self.resolve_alias(required_str(params, "pm")?)?;
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

    pub(super) fn rpc_job_list(self: &Arc<Self>, params: &Value) -> Result<Value> {
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
        let detail = params.get("tasks_detail").and_then(Value::as_bool) == Some(true);
        let mut out = Vec::new();
        for job in jobs {
            let mut j = job.to_json();
            let mut counts: std::collections::BTreeMap<String, i64> =
                std::collections::BTreeMap::new();
            let tasks = self.store.tasks_for_job(&job.id)?;
            for task in &tasks {
                *counts.entry(task.state.clone()).or_insert(0) += 1;
            }
            j["tasks"] = json!(counts);
            if detail {
                j["task_list"] = json!(tasks
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
            events.truncate(50);
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

    /// `job dispatch` — bookkeeping plus the kickoff enqueue in one
    /// store transaction. Does not bypass the ready gate: `--ready` is
    /// claimed client-side exactly like `send --ready`.
    pub(super) fn rpc_task_dispatch(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let to = optional_str(params, "to")
            .map(|a| self.resolve_alias(a))
            .transpose()?;
        let by = required_str(params, "by")?;
        self.plan_gate_task(required_str(params, "task")?)?;
        let (task, message, duplicate, behind_dead) = self.store.dispatch_task(
            required_str(params, "task")?,
            to.as_deref(),
            optional_str(params, "message"),
            by,
        )?;
        if let Some(assignee) = &task.assignee {
            self.notify_agent(assignee);
        }
        self.wake();
        Ok(json!({"task": task.to_json(), "message": message,
                  "duplicate": duplicate, "queued_behind_dead": behind_dead}))
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
        reject_identity_fields(params, "job task reopen")?;
        let task_id = required_str(params, "task")?;
        let caller = self.agent_caller(peer_pid, "job task reopen")?;
        if let AgentCaller::Agent(alias) = &caller {
            let task = self.store.task(task_id)?;
            let job = self.store.job(&task.job_id)?;
            let why = if task.assignee.as_deref() == Some(alias.as_str()) {
                Some(format!("agent '{alias}' is the task's assignee"))
            } else if job.pm_alias != *alias {
                Some(format!(
                    "agent '{alias}' is not job '{}''s PM ('{}')",
                    job.id, job.pm_alias
                ))
            } else {
                None
            };
            if let Some(why) = why {
                return Err(Error::rejected(format!(
                    "job task reopen refused: {why} — a task is reopened only by \
                     the operator or its job's own PM (caller rule, CAD-373)"
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
