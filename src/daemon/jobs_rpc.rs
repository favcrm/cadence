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
            self.store
                .dispatch_task_planned(&plan, delivery, by, expect.as_ref(), stall_secs)?;
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
                  "duplicate": duplicate, "queued_behind_dead": behind_dead}))
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
