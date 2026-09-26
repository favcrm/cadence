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
        }
        self.wake();
        Ok(out)
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
        let payload = json!({
            "project": project,
            "name": name,
            "digest": digest,
            "by": "operator",
            "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
            "notes": notes,
        });
        self.store.record_app_approval(payload.clone())?;
        self.wake();
        Ok(payload)
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
