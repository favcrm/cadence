//! CAD-534: `cadence daemon` plans RPC handlers — moved verbatim from src/daemon.rs; the
//! item→file map is src/daemon/split-map.toml
//! (scripts/split-daemon regenerates it).

use super::*;

impl Shared {
    /// CAD-360: the plan gate for every daemon dispatch of a task —
    /// `task_dispatch` and both monitor paths. A task whose job is bound
    /// to a tracker issue dispatches only if [`crate::issue::plan::gate_id`]
    /// passes. Fail closed: a task or job that cannot be read, or a
    /// tracker dir that cannot be resolved, refuses; only a job with no
    /// issue, or an issue no project holds, passes unchecked.
    pub(super) fn plan_gate_task(&self, task_id: &str) -> Result<()> {
        let task = self.store.task(task_id)?;
        let job = self.store.job(&task.job_id)?;
        let Some(issue) = job.issue_id else {
            return Ok(());
        };
        let pm_dir = self.pm_dir().map_err(|e| {
            Error::invalid(
                "plan_unreadable",
                format!("tracker dir for {issue} cannot be resolved ({e}) — refused"),
            )
        })?;
        crate::issue::plan::gate_id(&pm_dir, &issue)
    }

    /// CAD-359 `plan_propose` — write a plan (epic + tickets, one
    /// tracker commit) and emit `plan_proposed` on the daemon stream for
    /// a UI's plan card. The proposer is the connection's: a pane or
    /// managed endpoint's lane, else the proven operator; anything
    /// unattributable is refused, as is an identity-shaped field.
    pub(super) fn rpc_plan_propose(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        for field in [
            "by",
            "actor",
            "alias",
            "lane",
            "pane",
            "pid",
            "operator",
            "proposed_by",
        ] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "plan propose attribution is connection-bound; request field \
                     '{field}' is not accepted"
                )));
            }
        }
        let actor = match self.slot_identity(peer_pid)? {
            Some(who) => who.lane().to_string(),
            None => match self.operator_evidence(peer_pid) {
                Ok(()) => "operator".to_string(),
                Err(why) => {
                    return Err(Error::rejected(format!(
                        "plan propose needs an attributable caller — a pane agent, an \
                         enrolled managed endpoint or the proven operator: {why}"
                    )))
                }
            },
        };
        let project = required_str(params, "project")?;
        // `project` joins the pm dir on the workflow path — it is a key,
        // never a path fragment.
        crate::issue::model::check_key(project)?;
        let text = optional_str(params, "text");
        let workflow = optional_str(params, "workflow");
        let inputs = params.get("inputs");
        // CAD-577: a propose is a daemon touch of the app — reconcile
        // its derived grants with the current approval BEFORE the gate
        // runs. A structural change since approval revokes them (and
        // drains any waiting effect that lost a scope); a team change
        // re-derives. This runs even when the propose is then refused
        // `app_unapproved`, so the grants track the structure.
        if let Some((app, _)) = workflow.and_then(crate::issue::app::split_ref) {
            self.reconcile_app_grants(project, app);
        }
        let text = match (text, workflow) {
            (Some(t), None) => {
                if inputs.is_some() {
                    return Err(Error::rejected(
                        "'inputs' belong to a workflow proposal — `--workflow <name>`",
                    ));
                }
                t.to_string()
            }
            (None, Some(name)) => self.workflow_plan_text(project, name, inputs)?,
            _ => {
                return Err(Error::rejected(
                    "plan propose needs exactly one of 'text' or 'workflow' — \
                     `--file` or `--workflow`",
                ))
            }
        };
        let pm = self.pm()?;
        let allow = crate::secret::Allowlist::load(&self.state_dir)?;
        let out = crate::issue::plan::propose(&pm, project, &text, workflow, &allow, &actor)?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "plan_proposed",
            json!({
                "epic": out["epic"],
                "project": out["project"],
                "title": out["title"],
                "tickets": out["tickets"],
                "ticket_count": out["tickets"].as_array().map_or(0, Vec::len),
                "proposed_by": out["proposed_by"],
                "workflow": workflow,
                "app": workflow.and_then(|w| crate::issue::app::split_ref(w).map(|(a, _)| a)),
            }),
        );
        self.wake();
        Ok(out)
    }

    /// `plan propose --workflow <name>`: read
    /// `<pm>/<project>/workflows/<name>.md`, refuse unless its gate
    /// keys match the operator's recorded approval (a wording-only or
    /// structural edit alike self-unapproves until then), render it
    /// with `inputs` (a `{k: v}` map — missing required and unknown
    /// names refuse), and hand the rendered plan text to the ordinary
    /// propose path.
    fn workflow_plan_text(
        &self,
        project: &str,
        name: &str,
        inputs: Option<&Value>,
    ) -> Result<String> {
        let provided: std::collections::BTreeMap<String, String> = match inputs {
            None | Some(Value::Null) => Default::default(),
            Some(Value::Object(m)) => {
                let mut out = std::collections::BTreeMap::new();
                for (k, v) in m {
                    match v.as_str() {
                        Some(s) => {
                            out.insert(k.clone(), s.to_string());
                        }
                        None => {
                            return Err(Error::rejected(format!(
                                "input '{k}' must be a string — `--input {k}=<value>`"
                            )))
                        }
                    }
                }
                out
            }
            Some(_) => {
                return Err(Error::rejected(
                    "'inputs' must be an object of string values — `--input k=v`",
                ))
            }
        };
        let pm_dir = self.pm_dir()?;
        // `<app>/<workflow>` names an installed app's workflow (CAD-547):
        // the app's whole-bundle digest — slots and bindings included —
        // must match the operator's `app_approved` record. A bare name
        // stays the stored-workflow path.
        if let Some((app, wf)) = crate::issue::app::split_ref(name) {
            let approvals = self.store.app_approvals()?;
            return crate::issue::app::plan_text(&pm_dir, &approvals, project, app, wf, &provided);
        }
        if name.contains('/') {
            return Err(Error::rejected(format!(
                "workflow name '{name}' — a stored name, or <app>/<workflow> for an \
                 installed app (both [a-z0-9-], ≤32)"
            )));
        }
        let text = crate::issue::workflow::read_for(&pm_dir, project, name)?;
        let digest = crate::issue::workflow::gate_digest(&text)?;
        let approvals = self.store.workflow_approvals()?;
        let ok = approvals
            .get(&crate::issue::workflow::approval_key(project, name))
            .and_then(|p| p["digest"].as_str())
            == Some(digest.as_str());
        if !ok {
            return Err(Error::invalid(
                "workflow_unapproved",
                format!(
                    "workflow '{name}' in {project} is not approved for its current gate \
                     keys ({digest}) — an approval-affecting edit (agent, depends_on, \
                     size, reviewer, tries, uses, a ticket added or dropped) resets it; \
                     the operator re-approves with `cadence workflow approve {name} \
                     --project {project}`"
                ),
            ));
        }
        crate::issue::workflow::render(&text, &provided)
    }

    /// CAD-360 `plan_approve` / `plan_reject` — operator only, exactly
    /// the connection-bound rule of the approval-evidence verbs
    /// ([`Self::operator_connection`]): an agent caller is refused, and a
    /// caller with no agent identity must be the proven operator
    /// (CAD-276). The decision is a tracker commit; approval moves the
    /// plan's backlog tickets to ready.
    pub(super) fn rpc_plan_decide(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
        approve: bool,
    ) -> Result<Value> {
        let verb = if approve {
            "plan approve"
        } else {
            "plan reject"
        };
        self.operator_connection(verb, params, peer_pid)?;
        let epic = required_str(params, "epic")?;
        let pm = self.pm()?;
        let out = crate::issue::write::decide_plan(
            &pm,
            epic,
            approve,
            "operator",
            optional_str(params, "reason"),
        )?;
        let kind = if approve {
            "plan_approved"
        } else {
            "plan_rejected"
        };
        let _ = self.store.event_public(DAEMON_ALIAS, kind, out.clone());
        // CAD-445: the approved tickets are the master's to dispatch now.
        if approve {
            self.wake_on_plan_approved(&out);
            // CAD-577: resuming a run resumes the app's stopped team
            // agents instead of leaving their tasks queued.
            self.resume_run_team(epic);
        }
        self.wake();
        Ok(out)
    }

    /// CAD-577: when a plan is approved, resume the stopped agents the
    /// run's tickets are assigned to — the app's team. `agent resume`
    /// is the existing path; a live agent is left alone and a failure
    /// is recorded by the resume path itself (never a silent skip).
    /// Best-effort: a missing epic or an unreadable ticket just resumes
    /// what it can.
    pub(super) fn resume_run_team(self: &Arc<Self>, epic: &str) {
        let Ok(pm_dir) = self.pm_dir() else { return };
        let Ok(epic_issue) = crate::issue::board::find_issue(&pm_dir, epic) else {
            return;
        };
        let Some(plan) = &epic_issue.front.plan else {
            return;
        };
        let mut owners: Vec<String> = Vec::new();
        for id in &plan.tickets {
            if let Ok(issue) = crate::issue::board::find_issue(&pm_dir, id) {
                if let Some(owner) = issue.front.owner.clone() {
                    if !owners.contains(&owner) {
                        owners.push(owner);
                    }
                }
            }
        }
        for alias in owners {
            let Ok(agent) = self.store.agent(&alias) else {
                continue;
            };
            // Only a stopped agent with a real actor resumes; a live
            // one is left alone, an inbox has nothing to start.
            if agent.state != "stopped"
                || !crate::adapter::registry::has_actor(&agent.provider, &agent.endpoint_kind)
            {
                continue;
            }
            match self.try_resume(&alias) {
                Ok(true) => {
                    let _ = self.store.event_public(
                        &alias,
                        "run_team_resumed",
                        json!({"epic": epic, "reason": "the run's plan was approved"}),
                    );
                }
                Ok(false) => {}
                Err(e) => {
                    // A fenced or busy agent is not a silent skip —
                    // record why the resume did not start it.
                    let _ = self.store.event_public(
                        &alias,
                        "run_team_resume_failed",
                        json!({"epic": epic, "reason": e.to_string()}),
                    );
                }
            }
        }
    }

    /// CAD-405 `epic_stage` — move an epic's stage: a gate decision and
    /// one tracker commit ([`crate::issue::write::move_stage`]). A
    /// forward move into one of the project's `operator_stages`
    /// (default `build`, `release`) is operator only, by the same
    /// connection-bound rule as `plan approve`; any other move — the
    /// routine forward ones and every move back — is attributed to the
    /// caller's lane, or the proven operator, and an unattributable
    /// caller is refused. Identity-shaped fields are never read.
    ///
    /// CAD-432: `operator_decision: true` makes ANY move the operator's
    /// — the connection must pass `operator_connection` whatever the
    /// target. The board sets it on every relayed move: it relays over
    /// its own connection, so without it a board started under an agent
    /// would land the operator's routine moves as that agent's. The flag
    /// only narrows; it never grants.
    pub(super) fn rpc_epic_stage(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        for field in [
            "by",
            "actor",
            "alias",
            "lane",
            "pane",
            "pid",
            "operator",
            "recorded_via",
        ] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "stage move attribution is connection-bound; request field \
                     '{field}' is not accepted"
                )));
            }
        }
        let operator_decision = match params.get("operator_decision") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(Error::rejected("'operator_decision' must be a boolean")),
        };
        let epic = required_str(params, "epic")?;
        let stage = required_str(params, "stage")?;
        let note = optional_str(params, "note");
        let pm = self.pm()?;
        let approvals: crate::issue::work::Approvals = self
            .store
            .work_approvals()?
            .into_iter()
            .filter_map(|(k, v)| v["digest"].as_str().map(|d| (k, d.to_string())))
            .collect();
        let out = crate::issue::write::move_stage(&pm, epic, stage, note, &approvals, |mv| {
            if mv.needs_operator || operator_decision {
                self.operator_connection(
                    &format!("stage move into '{}'", mv.to),
                    params,
                    peer_pid,
                )?;
                return Ok("operator".to_string());
            }
            match self.slot_identity(peer_pid)? {
                Some(who) => Ok(who.lane().to_string()),
                None => self
                    .operator_evidence(peer_pid)
                    .map(|()| "operator".to_string())
                    .map_err(|why| {
                        Error::rejected(format!(
                            "stage move needs an attributable caller — a pane agent, an \
                             enrolled managed endpoint or the proven operator: {why}"
                        ))
                    }),
            }
        })?;
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "epic_stage_moved", out.clone());
        self.wake();
        Ok(out)
    }

    /// CAD-405 `project_work_approve` — the operator approves a
    /// project's PROJECT.md gate keys (`stages`, `operator_stages`) as
    /// they are now. The digest of the normalized keys is recorded in
    /// the daemon store with who and when (never read from a tracker
    /// commit, whose `git add -A` may sweep in an agent's edit); readers
    /// and stage moves apply the keys only while the file still matches
    /// it. Operator only, connection-bound like `plan approve`.
    pub(super) fn rpc_project_work_approve(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("project work approve", params, peer_pid)?;
        let project = required_str(params, "project")?;
        crate::issue::model::check_key(project)?;
        let pm_dir = self.pm_dir()?;
        if !crate::issue::project::list(&pm_dir)?
            .iter()
            .any(|p| p.key == project)
        {
            return Err(crate::issue::project::unknown_project(project, &pm_dir));
        }
        let cfg = crate::issue::work::load_config(&pm_dir, project)?;
        let payload = json!({
            "project": project,
            "digest": crate::issue::work::gate_digest(&cfg),
            "stages": cfg.stage_ids(),
            "operator_stages": cfg.operator_stages,
            "default": crate::issue::work::gates_default(&cfg),
            "by": "operator",
            "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
        });
        self.store.record_work_approval(payload.clone())?;
        self.wake();
        Ok(payload)
    }

    /// CAD-487 `workflow_approve` — the operator approves a workflow's
    /// gate keys (the ticket skeleton: `agent`, `depends_on`, `size`,
    /// `reviewer`, `tries`, `uses`) as they are now. The digest lands
    /// on the audit stream keyed `"<project>/<name>"`; `plan propose
    /// --workflow` matches the file's digest against it. Operator only,
    /// connection-bound like `plan approve`. A workflow that fails
    /// `workflow check` cannot be approved.
    pub(super) fn rpc_workflow_approve(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("workflow approve", params, peer_pid)?;
        let project = required_str(params, "project")?;
        crate::issue::model::check_key(project)?;
        let name = required_str(params, "name")?;
        let pm_dir = self.pm_dir()?;
        if !crate::issue::project::list(&pm_dir)?
            .iter()
            .any(|p| p.key == project)
        {
            return Err(crate::issue::project::unknown_project(project, &pm_dir));
        }
        let text = crate::issue::workflow::read_for(&pm_dir, project, name)?;
        let aliases: Vec<String> = self
            .store
            .agents()?
            .iter()
            .map(|a| a.alias.clone())
            .collect();
        let (agents, sources) =
            crate::issue::workflow::known_agents(&pm_dir, Some(project), &aliases);
        let (errors, notes, _) = crate::issue::workflow::check_text(&text, &agents, &sources);
        if !errors.is_empty() {
            return Err(Error::rejected(format!(
                "workflow '{name}' fails `workflow check` — approve it only after these \
                 are fixed: {}",
                errors.join("; ")
            )));
        }
        let payload = json!({
            "project": project,
            "name": name,
            "digest": crate::issue::workflow::gate_digest(&text)?,
            "by": "operator",
            "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
            "notes": notes,
        });
        self.store.record_workflow_approval(payload.clone())?;
        self.wake();
        Ok(payload)
    }

    /// CAD-577 `app_set_team` — the operator records an app's default
    /// team: one agent alias per workflow input role. Operator only,
    /// connection-bound like `app approve`. The team lives with the
    /// install record and is NOT in the gate digest, so setting it
    /// never re-requires approval; each role must be a team input the
    /// app's workflows declare, and each agent a registered alias.
    pub(super) fn rpc_app_set_team(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("app set team", params, peer_pid)?;
        let project = required_str(params, "project")?;
        crate::issue::model::check_key(project)?;
        let name = required_str(params, "name")?;
        let roles = match params.get("team") {
            Some(Value::Array(list)) => list
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| Error::rejected("'team' holds a non-string role"))
                })
                .collect::<Result<Vec<_>>>()?,
            Some(_) => {
                return Err(Error::rejected(
                    "'team' must be a list of '<input>=<agent>'",
                ))
            }
            None => return Err(Error::rejected("Missing or non-array 'team'")),
        };
        let pm_dir = self.pm_dir()?;
        if !crate::issue::project::list(&pm_dir)?
            .iter()
            .any(|p| p.key == project)
        {
            return Err(crate::issue::project::unknown_project(project, &pm_dir));
        }
        let pm = self.pm_at(&pm_dir)?;
        let out =
            crate::issue::app::set_team(&pm, project, name, &roles, &self.state_dir, "operator")?;
        // The team is what maps roles to agents, so a team change
        // re-derives the app's grants (CAD-577) — but only while the
        // app is still approved; a structural change revoked them.
        self.reconcile_app_grants(project, name);
        self.wake();
        Ok(out)
    }

    /// CAD-577 `app_add_worker` — the operator's one-click "Add
    /// worker": join a new Devin worker for one of the app's team
    /// roles, under the operator (a group root — its owner resolves to
    /// `operator`), with a unique role-prefixed alias, then record it
    /// in the app's team. The worker launches with the same defaults
    /// the operator's other Devin workers use (`auto_ready=verified`,
    /// `permission_mode=dangerous`). Operator only, connection-bound
    /// like `app approve`.
    pub(super) fn rpc_app_add_worker(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app add worker", params, peer_pid)?;
        let project = required_str(params, "project")?;
        crate::issue::model::check_key(project)?;
        let name = required_str(params, "name")?;
        let role = required_str(params, "role")?;
        let pm_dir = self.pm_dir()?;
        let projects = crate::issue::project::list(&pm_dir)?;
        let Some(proj) = projects.iter().find(|p| p.key == project) else {
            return Err(crate::issue::project::unknown_project(project, &pm_dir));
        };
        let roles = crate::issue::app::team_roles(&pm_dir, project, name)?;
        if !roles.iter().any(|r| r == role) {
            return Err(Error::rejected(format!(
                "app '{name}' has no team role '{role}' — its workflows name: {}",
                if roles.is_empty() {
                    "none".to_string()
                } else {
                    roles.join(", ")
                }
            )));
        }
        // The worker's checkout: the project's first repo path. A
        // project with no repo path cannot host a lane.
        let cwd = proj
            .repos
            .iter()
            .find_map(|r| r.path.as_deref())
            .map(crate::issue::project::expand_home)
            .filter(|p| p.is_dir())
            .ok_or_else(|| {
                Error::rejected(format!(
                    "project '{project}' names no repo path — a worker needs a \
                     checkout; add one with `cadence issue project`"
                ))
            })?;
        let alias = self.unique_worker_alias(role)?;
        // Register + launch through the ordinary path — the board's
        // proven operator is the caller, so `authorize_register` admits
        // it. No `upstream`: the worker is a group root the operator
        // owns (`inbox::owner_of` answers `operator`).
        let register = json!({
            "alias": alias,
            "provider": "devin",
            "endpoint_kind": "pty",
            "role": "worker",
            "cwd": cwd.to_string_lossy(),
            "params": json!({"auto_ready": "verified",
                               "permission_mode": "dangerous"}).to_string(),
        });
        self.rpc_register(&register, peer_pid)?;
        // Record the new worker in the app's team for that role — the
        // operator's own write, so the New post drawer pre-fills it.
        let pm = self.pm_at(&pm_dir)?;
        let out = crate::issue::app::set_team(
            &pm,
            project,
            name,
            &[format!("{role}={alias}")],
            &self.state_dir,
            "operator",
        )?;
        self.reconcile_app_grants(project, name);
        self.wake();
        Ok(json!({"alias": alias, "role": role, "team": out["team"]}))
    }

    /// A unique role-prefixed worker alias (`<role>-<6 hex>`) — the
    /// prefix names the role, the suffix keeps it unique (CAD-577).
    fn unique_worker_alias(&self, role: &str) -> Result<String> {
        for _ in 0..32 {
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let alias = format!("{role}-{}", &suffix[..6]);
            if self.store.agent_opt(&alias)?.is_none() {
                return Ok(alias);
            }
        }
        Err(Error::internal("could not mint a unique worker alias"))
    }

    /// Reconcile one app's derived grants with its current approval
    /// (CAD-577). The store re-reads the approval under the write lock,
    /// so a revoke that landed after this call computed the digest
    /// cannot be overwritten by the stale derivation. An app whose
    /// folder is gone is revoked here too — removal deletes the tracker
    /// files, and a later propose of anything else sweeps the leftovers.
    pub(super) fn reconcile_app_grants(&self, project: &str, name: &str) {
        self.sweep_removed_app_grants();
        let Ok(pm_dir) = self.pm_dir() else {
            return;
        };
        let key = crate::issue::app::approval_key(project, name);
        if !crate::issue::app::is_installed(&pm_dir, project, name) {
            if let Ok(changed) = self.store.app_grants_reconcile(&key, None, &[], "operator") {
                self.drain_effect_scopes(changed);
            }
            return;
        }
        let Ok(digest) = crate::issue::app::digest(&pm_dir, project, name) else {
            return;
        };
        let Ok(derived) = crate::issue::app::derive_grants(&pm_dir, project, name) else {
            return;
        };
        let grants: Vec<(String, String, String, Vec<String>)> = derived
            .iter()
            .map(|g| {
                (
                    g.agent.clone(),
                    g.platform.clone(),
                    g.account.clone(),
                    g.scopes.clone(),
                )
            })
            .collect();
        if let Ok(changed) =
            self.store
                .app_grants_reconcile(&key, Some(digest.as_str()), &grants, "operator")
        {
            self.drain_effect_scopes(changed);
        }
    }

    /// Revoke derived grants whose app folder is gone (CAD-577). A
    /// removal that raced a propose, or a folder deleted by hand, leaves
    /// `app_grants` rows nothing else would notice. The same write
    /// records a withdrawn approval: dropping the rows alone leaves a
    /// reinstall of the same digest already approved.
    fn sweep_removed_app_grants(&self) {
        let Ok(pm_dir) = self.pm_dir() else {
            return;
        };
        let Ok(apps) = self.store.app_grants_apps() else {
            return;
        };
        for key in apps {
            let Some((project, name)) = key.split_once('/') else {
                continue;
            };
            if crate::issue::app::is_installed(&pm_dir, project, name) {
                continue;
            }
            let payload = json!({
                "project": project,
                "name": name,
                "digest": Value::Null,
                "revoked": true,
                "by": "operator",
                "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
                "reason": "removed",
            });
            if let Ok(changed) = self.store.app_revoke_with_record(payload, &key, "operator") {
                self.drain_effect_scopes(changed);
            }
        }
    }

    /// Close the waiting effects of the `(agent, platform, account)`
    /// triples whose grant changed, when the row's frozen scopes are no
    /// longer covered (CAD-506's rule, reused by the app-grant revoke).
    fn drain_effect_scopes(&self, changed: Vec<(String, String, String)>) {
        let mut closed_any = false;
        for (agent, platform, account) in changed {
            let surviving = self
                .store
                .platform_grant(&agent, &platform, &account)
                .ok()
                .flatten();
            let stranded: Vec<(String, String)> = self
                .store
                .platform_effects(Some(&agent))
                .unwrap_or_default()
                .into_iter()
                .filter(|r| {
                    r.platform == platform
                        && r.account == account
                        && r.state == "waiting"
                        && r.scopes
                            .iter()
                            .any(|s| surviving.as_ref().is_none_or(|g| !g.covers(s)))
                })
                .map(|r| (r.request, r.effect_id))
                .collect();
            for (request, effect_id) in stranded {
                if let Ok(Some(row)) = self
                    .store
                    .effect_close(crate::store::EffectKey::Id(effect_id), "grant_revoked")
                {
                    let _ = self.store.event_public(
                        &row.agent,
                        "request_closed",
                        json!({"request": request, "kind": "effect",
                               "reason": "grant_revoked"}),
                    );
                    closed_any = true;
                }
            }
        }
        if closed_any {
            self.wake();
        }
    }

    /// CAD-577 `app_revoke` — the operator revokes an app's approval:
    /// the `app_approved` record is superseded (a `revoked` event with
    /// no digest, so `plan propose` refuses again) and every grant the
    /// approval derived is revoked, draining any waiting effect that
    /// lost a scope. Operator only, connection-bound like `app approve`.
    pub(super) fn rpc_app_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("app revoke", params, peer_pid)?;
        let project = required_str(params, "project")?;
        crate::issue::model::check_key(project)?;
        let name = required_str(params, "name")?;
        let key = crate::issue::app::approval_key(project, name);
        let payload = json!({
            "project": project,
            "name": name,
            "digest": Value::Null,
            "revoked": true,
            "by": "operator",
            "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
        });
        // The same tracker write lock approve holds, taken before the
        // store write. An approve that has already derived its grants
        // cannot commit afterwards and put them back.
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let _lock = pm.lock()?;
        // The approval write and the grant subtract share one
        // transaction, so a reconcile cannot re-derive between them.
        let changed = self
            .store
            .app_revoke_with_record(payload, &key, "operator")?;
        self.drain_effect_scopes(changed);
        self.wake();
        Ok(json!({"project": project, "name": name, "revoked": true}))
    }

    /// CAD-547 `app_approve` — the operator approves an installed app's
    /// structural digest: the manifest envelope (name, declared slots),
    /// each slot's effective binding, the `app.md` guide, every
    /// workflow's CAD-487 gate digest, and every rubric/template byte —
    /// everything `plan propose --workflow <app>/<wf>` matches before
    /// it renders. Operator only, connection-bound like
    /// `workflow approve`. The INSTALLED folder is re-verified with the
    /// same checks `app install` ran, so a hand edit after install
    /// cannot smuggle content past the review; anything that fails
    /// those checks refuses the approval.
    pub(super) fn rpc_app_approve(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("app approve", params, peer_pid)?;
        let project = required_str(params, "project")?;
        crate::issue::model::check_key(project)?;
        let name = required_str(params, "name")?;
        let pm_dir = self.pm_dir()?;
        if !crate::issue::project::list(&pm_dir)?
            .iter()
            .any(|p| p.key == project)
        {
            return Err(crate::issue::project::unknown_project(project, &pm_dir));
        }
        let aliases: Vec<String> = self
            .store
            .agents()?
            .iter()
            .map(|a| a.alias.clone())
            .collect();
        let (agents, sources) =
            crate::issue::workflow::known_agents(&pm_dir, Some(project), &aliases);
        // The tracker write lock `app update`/`install` also take: the
        // installed bundle must not change under check_installed and
        // digest, or the approval could pin a half-updated read (N6).
        let pm = self.pm_at(&pm_dir)?;
        let _lock = pm.lock()?;
        let notes = crate::issue::app::check_installed(&pm_dir, project, name, &agents, &sources)
            .map_err(|e| {
            Error::rejected(format!(
                "app '{name}' fails the install checks — approve it only after \
                     these are fixed (`cadence app show {name} --project {project}`): \
                     {e}"
            ))
        })?;
        let digest = crate::issue::app::digest(&pm_dir, project, name)?;
        // CAD-577: the operator's one Approve derives the app's grants —
        // exactly the scopes its workflow steps declare on their bound
        // slots, to the agents the app's default team assigns those
        // steps. The derivation records on the audit stream; a
        // re-approval re-derives, so a dropped scope goes with it.
        let derived = crate::issue::app::derive_grants(&pm_dir, project, name)?;
        let grants: Vec<(String, String, String, Vec<String>)> = derived
            .iter()
            .map(|g| {
                (
                    g.agent.clone(),
                    g.platform.clone(),
                    g.account.clone(),
                    g.scopes.clone(),
                )
            })
            .collect();
        let key = crate::issue::app::approval_key(project, name);
        let payload = json!({
            "project": project,
            "name": name,
            "digest": digest,
            "by": "operator",
            "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
            "notes": notes,
            "grants": derived.iter().map(|g| json!({
                "agent": g.agent, "platform": g.platform,
                "account": g.account, "scopes": g.scopes,
            })).collect::<Vec<_>>(),
        });
        // Approval and grants land together. A re-approval subtracts
        // the previous derivation inside that same write. The pause
        // sits in that gap, still holding the tracker lock, so a
        // revoke on another connection can be shown to wait.
        self.pause_approve_inside_lock();
        let changed =
            self.store
                .app_approve_with_grants(payload.clone(), &key, &grants, "operator")?;
        self.drain_effect_scopes(changed);
        self.wake();
        Ok(payload)
    }

    /// Test seam for the approve/revoke race. `CADENCE_TEST_APP_APPROVE_HOLD`
    /// is this daemon's own env (`ProviderEnv::own`), never the process
    /// environment. While that path exists, an approve that has already
    /// derived its grants stays inside the tracker write lock. It writes
    /// `{path}.ready` on the way in. The test deletes the hold to let
    /// the approve finish. Absent the variable, this is a no-op.
    fn pause_approve_inside_lock(&self) {
        let Some(path) = self.provider_env.own("CADENCE_TEST_APP_APPROVE_HOLD") else {
            return;
        };
        if path.is_empty() {
            return;
        }
        let ready = format!("{path}.ready");
        let _ = std::fs::write(&ready, "1");
        let deadline = Instant::now() + Duration::from_secs(12);
        while Path::new(&path).exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// CAD-358 `project_new` — register a repo as a project and seed its
    /// PROJECT.md in one tracker commit ([`crate::issue::project_new`]).
    /// Callers: the proven operator, and the master (CAD-339) by its
    /// verified connection ([`Self::caller_is_master`]) — never by a
    /// request field. Identity-shaped fields are refused FIRST, for every
    /// caller, so the master path never skips that refusal. Every other
    /// agent — a pane, a managed endpoint or its tool subprocess — and a
    /// detached child of any agent, the master included, is refused
    /// (`operator_connection`). On both paths the tracker and this
    /// daemon's state dir are refused as the repo.
    pub(super) fn rpc_project_new(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_identity_fields(params, "project new")?;
        reject_operator_fields("project new", params)?;
        let actor = if self.caller_is_master(peer_pid) {
            crate::master::ALIAS
        } else {
            self.operator_connection("project new", params, peer_pid)?;
            "operator"
        };
        let text = |name: &str| optional_str(params, name).map(str::to_string);
        let agents = match params.get("agents") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(list)) => list
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| Error::rejected("'agents' must be a list of strings"))
                })
                .collect::<Result<_>>()?,
            Some(_) => return Err(Error::rejected("'agents' must be a list of strings")),
        };
        let req = crate::issue::project_new::Request {
            key: required_str(params, "key")?.to_string(),
            repo: std::path::PathBuf::from(required_str(params, "repo")?),
            prefix: text("prefix"),
            goal: text("goal"),
            agents,
            issue: text("issue"),
        };
        let pm = self.pm()?;
        let out = crate::issue::project_new::run(
            &pm,
            &req,
            actor,
            &[("the daemon state dir", self.state_dir.as_path())],
        )?;
        if out["changed"] == true {
            let _ = self
                .store
                .event_public(DAEMON_ALIAS, "project_registered", out.clone());
            self.wake();
        }
        Ok(out)
    }
}
