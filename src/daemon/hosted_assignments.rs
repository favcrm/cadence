//! Explicit operator configuration and daemon-owned workspace event workers.
use super::*;
use crate::local_assignment::{events, reporting::ReportDriver, ResumeSpec, SpawnSpec};
use crate::remote_cli::RemoteTarget;

impl Shared {
    fn spawn_hosted_worker(
        self: &Arc<Self>,
        spec: &SpawnSpec,
        worktree: &Path,
    ) -> Result<(String, String)> {
        let provider = spec.provider.as_str();
        let kind = crate::adapter::registry::default_kind(provider)?;
        crate::adapter::registry::refuse_reviewer_pane(provider, kind, "worker")?;
        self.kickoff_launch_policy(
            provider,
            kind,
            spec.model.as_deref(),
            spec.effort.as_deref(),
        )?;

        let mut alias = String::new();
        for _ in 0..8 {
            let suffix = &Uuid::new_v4().simple().to_string()[..6];
            let cand = format!("{provider}-{suffix}");
            if self.store.agent_opt(&cand)?.is_none()
                && crate::master::refuse_reserved_alias(&cand).is_ok()
                && crate::agent_settings::refuse_profile_alias(&self.store, &cand).is_ok()
            {
                alias = cand;
                break;
            }
        }
        if alias.is_empty() {
            return Err(Error::rejected(
                "hosted spawn: could not mint a free worker alias",
            ));
        }

        let mut launch = serde_json::Map::new();
        if let Some(model) = spec.model.as_deref() {
            launch.insert("model".to_string(), json!(model));
        }
        if let Some(effort) = spec.effort.as_deref() {
            launch.insert("effort".to_string(), json!(effort));
        }
        let launch_val = Value::Object(launch.clone());
        if !launch.is_empty() {
            crate::adapter::registry::validate_launch_params(provider, kind, &launch_val)?;
        }
        let params_str = (!launch.is_empty()).then(|| launch_val.to_string());
        let profile_instructions = spawn_agent_instructions(spec.profile_id.as_deref());
        let cwd = std::fs::canonicalize(worktree)
            .map_err(|_| Error::rejected("Working directory must exist"))?;
        self.store.register_agent(&crate::store::NewAgent {
            alias: &alias,
            provider,
            endpoint_kind: kind,
            role: "worker",
            cwd: &cwd.to_string_lossy(),
            sandbox: "read-only",
            instructions: profile_instructions.as_deref(),
            params: params_str.as_deref(),
            team_role: None,
            model_policy: None,
        })?;
        self.launch_actor(&alias)?;
        Ok((alias, format!("{provider}/{kind}")))
    }

    pub(super) fn resume_hosted_worker(
        self: &Arc<Self>,
        spec: &ResumeSpec,
        alias: &str,
        worktree: &Path,
        is_seat: bool,
    ) -> Result<String> {
        let agent = self.store.agent_opt(alias)?.ok_or_else(|| {
            Error::invalid(
                "resume_agent_not_found",
                format!("local agent '{alias}' is no longer registered"),
            )
        })?;
        if agent.archived_at.is_some() {
            return Err(Error::invalid(
                "resume_agent_not_found",
                format!("local agent '{alias}' is archived"),
            ));
        }
        let agent_cwd = std::fs::canonicalize(Path::new(&agent.cwd)).map_err(|_| {
            Error::invalid(
                "resume_worktree_missing",
                format!("working directory for local agent '{alias}' no longer exists"),
            )
        })?;
        let expected_wt = std::fs::canonicalize(worktree).map_err(|_| {
            Error::invalid(
                "resume_worktree_missing",
                "recorded lane worktree directory no longer exists",
            )
        })?;
        if is_seat {
            let agent_root = crate::worktree::main_root(&agent_cwd).map_err(|_| {
                Error::invalid(
                    "resume_worktree_mismatch",
                    format!("seat agent '{alias}' cwd is not a git repository"),
                )
            })?;
            let expected_root = crate::worktree::main_root(&expected_wt).map_err(|_| {
                Error::invalid(
                    "resume_worktree_mismatch",
                    "recorded lane worktree is not inside a git repository",
                )
            })?;
            if agent_root != expected_root {
                return Err(Error::invalid(
                    "resume_worktree_mismatch",
                    format!("seat agent '{alias}' repository does not match the recorded lane"),
                ));
            }
        } else if agent_cwd != expected_wt {
            return Err(Error::invalid(
                "resume_worktree_mismatch",
                format!("spawned worker '{alias}' cwd does not match the recorded worktree"),
            ));
        }
        refuse_fenced_default(alias, &agent.state, self.store.has_unknown(alias)?)?;

        let mut launch = serde_json::Map::new();
        if spec.model.is_some() || spec.effort.is_some() {
            self.kickoff_launch_policy(
                &agent.provider,
                &agent.endpoint_kind,
                spec.model.as_deref(),
                spec.effort.as_deref(),
            )?;
            if let Some(model) = spec.model.as_deref() {
                launch.insert("model".to_string(), json!(model));
            }
            if let Some(effort) = spec.effort.as_deref() {
                launch.insert("effort".to_string(), json!(effort));
            }
            crate::adapter::registry::validate_launch_params(
                &agent.provider,
                &agent.endpoint_kind,
                &Value::Object(launch.clone()),
            )?;
        }

        let owned = self.lifecycle.lock().unwrap().owned(alias);
        if owned {
            let live = agent.endpoint.is_some()
                || matches!(agent.state.as_str(), "idle" | "running" | "waiting_input");
            if !live {
                return Err(Error::busy_coded(
                    "agent_transitioning",
                    format!("local agent '{alias}' is still starting or stopping; retry shortly"),
                ));
            }
            let current_model = agent
                .params
                .as_ref()
                .and_then(|p| p.get("model"))
                .and_then(Value::as_str);
            let current_effort = agent
                .params
                .as_ref()
                .and_then(|p| p.get("effort"))
                .and_then(Value::as_str);
            let model_changed = spec
                .model
                .as_deref()
                .is_some_and(|m| Some(m) != current_model);
            let effort_changed = spec
                .effort
                .as_deref()
                .is_some_and(|e| Some(e) != current_effort);
            if model_changed || effort_changed {
                return Err(Error::invalid(
                    "agent_live_override_refused",
                    format!(
                        "local agent '{alias}' is currently live; stop it or omit model/effort changes to resume its active session"
                    ),
                ));
            }
        } else {
            if !launch.is_empty() {
                self.store.set_params(alias, &Value::Object(launch))?;
            }
            let started = self.try_resume(alias)?;
            if !started {
                return Err(Error::invalid(
                    "resume_failed",
                    format!("local agent '{alias}' could not be resumed"),
                ));
            }
        }
        Ok(format!("{}/{}", agent.provider, agent.endpoint_kind))
    }

    pub(super) fn rpc_hosted_local_agent(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection_on_agent("hosted local agent binding", params, peer_pid)?;
        let org_id = required_str(params, "organization_id")?;
        let value = match method {
            "hosted_local_agent_connect" => {
                let alias = self.resolve_alias(required_str(params, "alias")?)?;
                let agent = self.store.agent(&alias)?;
                let target = RemoteTarget {
                    org: required_str(params, "org")?.to_string(),
                    org_id: org_id.to_string(),
                    endpoint: required_str(params, "endpoint")?.to_string(),
                    identity: None,
                };
                let fyi_only = params
                    .get("fyi_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let spawn_policy = match params.get("allow_spawn").and_then(Value::as_bool) {
                    Some(true) => {
                        let repos = params
                            .get("spawn_repos")
                            .and_then(Value::as_array)
                            .map(|rows| {
                                rows.iter()
                                    .filter_map(|row| {
                                        let name = row.get("name").and_then(Value::as_str)?;
                                        let path = row.get("path").and_then(Value::as_str)?;
                                        Some(events::SpawnRepoBinding {
                                            name: name.to_string(),
                                            path: path.to_string(),
                                        })
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        let harnesses = params
                            .get("harnesses")
                            .and_then(Value::as_array)
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .filter(|h| !h.is_empty())
                            .unwrap_or_else(|| {
                                vec![
                                    "omp".to_string(),
                                    "pi".to_string(),
                                    "claude".to_string(),
                                    "codex".to_string(),
                                ]
                            });
                        let max_concurrent = params
                            .get("max_concurrent")
                            .and_then(Value::as_u64)
                            .and_then(|n| u32::try_from(n).ok())
                            .unwrap_or(2);
                        Some(events::SpawnPolicy {
                            spawn_allowed: true,
                            repos,
                            harnesses,
                            max_concurrent,
                        })
                    }
                    Some(false) => Some(events::SpawnPolicy {
                        spawn_allowed: false,
                        repos: Vec::new(),
                        harnesses: Vec::new(),
                        max_concurrent: 0,
                    }),
                    None => None,
                };
                events::connect(
                    &self.state_dir,
                    &target,
                    &agent,
                    fyi_only,
                    optional_str(params, "name"),
                    optional_str(params, "repo").map(Path::new),
                    spawn_policy,
                )?
            }
            "hosted_local_agent_disconnect" => events::disconnect(&self.state_dir, org_id)?,
            "hosted_local_agent_observe" => {
                let ticket = self.changed.ticket();
                let mut value = events::observe(
                    &self.state_dir,
                    org_id,
                    params
                        .get("clear")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )?;
                if params.get("wait").and_then(Value::as_bool) == Some(true)
                    && value["cursor"].as_u64() == params.get("after").and_then(Value::as_u64)
                {
                    self.changed
                        .wait_if_unchanged(ticket, Instant::now() + Duration::from_secs(30));
                    value = events::observe(&self.state_dir, org_id, false)?;
                }
                return Ok(value);
            }
            _ => return Err(Error::rejected("unknown hosted local agent method")),
        };
        self.wake();
        Ok(value)
    }

    pub(super) fn run_hosted_assignments(self: Arc<Self>) {
        let reporter = ReportDriver::new(&self.state_dir);
        let mut workers: Vec<(events::WorkspaceBinding, Arc<AtomicBool>)> = Vec::new();
        while !self.closing.load(Ordering::SeqCst) {
            // Capture before examining state: a concurrent outcome/config change
            // cannot be lost between the scan and the condition-variable wait.
            let ticket = self.changed.ticket();
            if !self.execution_mode.fenced() {
                if let Ok(bindings) = events::bindings(&self.state_dir) {
                    workers.retain(|(binding, stop)| {
                        let keep = bindings.iter().any(|current| current == binding);
                        if !keep {
                            stop.store(true, Ordering::SeqCst);
                        }
                        keep
                    });
                    for binding in bindings {
                        if workers.iter().any(|(current, _)| current == &binding) {
                            continue;
                        }
                        let stop = Arc::new(AtomicBool::new(false));
                        workers.push((binding.clone(), stop.clone()));
                        let state = self.state_dir.clone();
                        let active_lanes =
                            Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
                                String,
                                ActiveHostedLane,
                            >::new()));

                        let shared_send = self.clone();
                        let send_stop = stop.clone();
                        let send_binding = binding.clone();
                        let default_alias = binding.default_alias().map(str::to_string);
                        let active_for_send = Arc::clone(&active_lanes);
                        let send = Arc::new(move |params: Value| {
                            let target_alias = params["alias"].as_str();
                            let allowed_alias = target_alias.is_some_and(|alias| {
                                default_alias.as_deref() == Some(alias)
                                    || send_binding.agents().iter().any(|a| a.alias == alias)
                                    || active_for_send
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .values()
                                        .any(|lane| lane.alias == alias)
                            });
                            if send_stop.load(Ordering::SeqCst)
                                || shared_send.closing.load(Ordering::SeqCst)
                                || shared_send.execution_mode.fenced()
                                || !allowed_alias
                            {
                                return Err(Error::busy_coded(
                                    "binding_cancelled",
                                    "hosted assignment binding is no longer active",
                                ));
                            }
                            // Validate the persisted selection again immediately before send.
                            if !events::bindings(&shared_send.state_dir)?
                                .iter()
                                .any(|current| current == &send_binding)
                            {
                                return Err(Error::busy_coded(
                                    "binding_cancelled",
                                    "hosted workspace was disconnected or rebound",
                                ));
                            }
                            shared_send.send_hosted_kickoff(&params)
                        });

                        let shared_snap = self.clone();
                        let active_for_snap = Arc::clone(&active_lanes);
                        let snapshot = Arc::new(move |b: &events::WorkspaceBinding| {
                            let mut agents = std::collections::BTreeMap::new();
                            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                            let lanes = active_for_snap.lock().unwrap_or_else(|e| e.into_inner());
                            for item in b.agents() {
                                let row_state = shared_snap
                                    .store
                                    .agent_opt(&item.alias)
                                    .ok()
                                    .flatten()
                                    .map(|row| row.state);
                                let issue_id = active_issue_for_alias(&lanes, &item.alias);
                                agents.insert(
                                    item.alias.clone(),
                                    build_agent_activity_snapshot(
                                        row_state.as_deref(),
                                        issue_id,
                                        &now,
                                    ),
                                );
                            }
                            let active_spawn_lanes =
                                lanes.values().filter(|lane| lane.spawned).count() as u32;
                            events::RunnerRuntimeSnapshot {
                                agents,
                                active_spawn_lanes,
                            }
                        });

                        let shared_spawn = self.clone();
                        let active_for_spawn = Arc::clone(&active_lanes);
                        let max_spawn_lanes = binding
                            .spawn_policy()
                            .filter(|policy| policy.spawn_allowed)
                            .map(|policy| policy.max_concurrent)
                            .unwrap_or(0);
                        let spawn_worker = Arc::new(
                            move |spec: &SpawnSpec, worktree: &Path, assignment_id: &str| {
                                {
                                    let mut lanes =
                                        active_for_spawn.lock().unwrap_or_else(|e| e.into_inner());
                                    reserve_spawn_slot(&mut lanes, assignment_id, max_spawn_lanes)?;
                                }
                                let spawned = shared_spawn.spawn_hosted_worker(spec, worktree);
                                let mut lanes =
                                    active_for_spawn.lock().unwrap_or_else(|e| e.into_inner());
                                match spawned {
                                    Ok((alias, adapter)) => {
                                        if let Some(entry) = lanes.get_mut(assignment_id) {
                                            entry.alias = alias.clone();
                                        }
                                        Ok((alias, adapter))
                                    }
                                    Err(err) => {
                                        lanes.remove(assignment_id);
                                        Err(err)
                                    }
                                }
                            },
                        );

                        let shared_resume = self.clone();
                        let active_for_resume = Arc::clone(&active_lanes);
                        let resume_worker: events::ResumeWorkerFn = Arc::new(
                            move |spec: &ResumeSpec,
                                  alias: &str,
                                  worktree: &Path,
                                  assignment_id: &str,
                                  is_seat: bool| {
                                if !is_seat {
                                    let mut lanes =
                                        active_for_resume.lock().unwrap_or_else(|e| e.into_inner());
                                    reserve_spawn_slot(&mut lanes, assignment_id, max_spawn_lanes)?;
                                    if let Some(entry) = lanes.get_mut(assignment_id) {
                                        entry.alias = alias.to_string();
                                    }
                                }
                                match shared_resume
                                    .resume_hosted_worker(spec, alias, worktree, is_seat)
                                {
                                    Ok(adapter) => Ok(adapter),
                                    Err(err) => {
                                        if !is_seat {
                                            active_for_resume
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .remove(assignment_id);
                                        }
                                        Err(err)
                                    }
                                }
                            },
                        );

                        let active_for_kickoff = Arc::clone(&active_lanes);
                        let on_kickoff =
                            Arc::new(move |alias: &str, assignment_id: &str, issue_id: &str| {
                                let mut lanes =
                                    active_for_kickoff.lock().unwrap_or_else(|e| e.into_inner());
                                record_hosted_kickoff(&mut lanes, alias, assignment_id, issue_id);
                            });

                        let active_for_term = Arc::clone(&active_lanes);
                        let on_terminal = Arc::new(move |assignment_id: &str| {
                            active_for_term
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .remove(assignment_id);
                        });

                        let notifier = self.clone();
                        let notify = Arc::new(move || notifier.wake());
                        let runtime = events::EventRuntime {
                            send,
                            snapshot,
                            spawn_worker,
                            resume_worker,
                            on_kickoff,
                            on_terminal,
                            notify,
                        };
                        thread::spawn(move || events::run(state, binding, stop, runtime));
                    }
                }
                let _ = reporter.tick(&self.store, Instant::now() + Duration::from_secs(5));
            }
            let pending = reporter.has_pending().unwrap_or(false);
            let deadline = Instant::now()
                + if pending {
                    Duration::from_secs(5)
                } else {
                    Duration::from_secs(24 * 60 * 60)
                };
            self.changed.wait_if_unchanged(ticket, deadline);
        }
        for (_, stop) in workers {
            stop.store(true, Ordering::SeqCst);
        }
    }

    /// The send a workspace worker makes for a hosted kickoff to the bound
    /// default agent, after its binding checks. A fenced default is refused
    /// first (CAD-2259); otherwise this is the existing agent_send, without
    /// operator or steering authority.
    pub(super) fn send_hosted_kickoff(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        refuse_fenced_default(
            alias,
            &self.store.agent(alias)?.state,
            self.store.has_unknown(alias)?,
        )?;
        self.send_as(params, &|_| Ok(store::Sender::Unattributed))
    }
}

/// CAD-2259: a fenced default agent (`attention`, or holding an unknown
/// message) runs nothing until the operator resumes or unfences it, so a
/// kickoff sent to it would only queue while the board shows the assignment
/// `running`. This refusal is not `binding_cancelled`, so the coordinator
/// reports the claim `blocked` with this text instead. Only these persisted
/// states count: right after a daemon start every enabled managed agent
/// still reads as dead until relaunch, and an idle-stopped agent is
/// auto-resumed for queued work (CAD-413), so neither is refused here.
fn refuse_fenced_default(alias: &str, state: &str, has_unknown: bool) -> Result<()> {
    if state != "attention" && !has_unknown {
        return Ok(());
    }
    Err(Error::invalid(
        "default_agent_unavailable",
        format!(
            "The local agent {alias} is fenced and needs the operator, so this kickoff was \
             not queued. Resume or unfence it, or connect another default agent, then \
             connect again to pick this assignment up."
        ),
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveHostedLane {
    alias: String,
    issue_id: String,
    spawned: bool,
}

fn spawn_agent_instructions(profile_id: Option<&str>) -> Option<String> {
    profile_id.map(|id| format!("Hosted agent profile: {id}"))
}

fn reserve_spawn_slot(
    lanes: &mut std::collections::BTreeMap<String, ActiveHostedLane>,
    assignment_id: &str,
    max_concurrent: u32,
) -> Result<()> {
    let active = lanes
        .values()
        .enumerate()
        .filter(|(_, lane)| lane.spawned)
        .count() as u32;
    let already_reserved = lanes.get(assignment_id).is_some_and(|lane| lane.spawned);
    if !already_reserved && active >= max_concurrent {
        return Err(Error::busy_coded(
            "runner_at_capacity",
            "local runner reached max concurrent spawned lanes",
        ));
    }
    lanes
        .entry(assignment_id.to_string())
        .or_insert_with(|| ActiveHostedLane {
            alias: String::new(),
            issue_id: String::new(),
            spawned: true,
        })
        .spawned = true;
    Ok(())
}

fn record_hosted_kickoff(
    lanes: &mut std::collections::BTreeMap<String, ActiveHostedLane>,
    alias: &str,
    assignment_id: &str,
    issue_id: &str,
) {
    match lanes.get_mut(assignment_id) {
        Some(existing) => {
            existing.alias = alias.to_string();
            existing.issue_id = issue_id.to_string();
        }
        None => {
            lanes.insert(
                assignment_id.to_string(),
                ActiveHostedLane {
                    alias: alias.to_string(),
                    issue_id: issue_id.to_string(),
                    spawned: false,
                },
            );
        }
    }
}

fn active_issue_for_alias(
    lanes: &std::collections::BTreeMap<String, ActiveHostedLane>,
    alias: &str,
) -> Option<String> {
    lanes
        .values()
        .rev()
        .find(|lane| lane.alias == alias && !lane.issue_id.is_empty())
        .map(|lane| lane.issue_id.clone())
}

fn build_agent_activity_snapshot(
    row_state: Option<&str>,
    issue_id: Option<String>,
    now: &str,
) -> events::AgentActivitySnapshot {
    let status = match row_state {
        Some("busy" | "starting") => "busy",
        Some("attention") => "blocked",
        Some("idle") => "idle",
        _ => "offline",
    };
    let summary = match (status, issue_id.as_deref()) {
        ("busy", Some(id)) => format!("Working on {id}"),
        ("busy", None) => "Busy".to_string(),
        ("blocked", Some(id)) => format!("Needs attention on {id}"),
        ("blocked", None) => "Needs attention".to_string(),
        ("idle", Some(id)) => format!("Assigned to {id}"),
        ("idle", None) => "Idle".to_string(),
        (_, Some(id)) => format!("Offline ({id})"),
        (_, None) => "Offline".to_string(),
    };
    events::AgentActivitySnapshot {
        status: status.to_string(),
        summary: Some(summary),
        issue_id,
        updated_at: Some(now.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        active_issue_for_alias, build_agent_activity_snapshot, record_hosted_kickoff,
        refuse_fenced_default, reserve_spawn_slot, spawn_agent_instructions, ActiveHostedLane,
        ServeOptions, Shared,
    };
    use std::collections::BTreeMap;

    #[test]
    fn cad2259_a_fenced_default_refuses_the_kickoff_with_its_reason() {
        for (state, unknown) in [("attention", false), ("attention", true), ("idle", true)] {
            let error = refuse_fenced_default("roster-a", state, unknown).unwrap_err();
            assert_eq!(error.kind(), "rejected", "{state}/{unknown}");
            assert_eq!(error.code(), Some("default_agent_unavailable"));
            let text = error.to_string();
            assert!(
                text.contains("roster-a") && text.contains("not queued"),
                "{text}"
            );
        }
    }

    #[test]
    fn cad2259_a_live_or_stopped_default_still_takes_the_kickoff() {
        for state in ["idle", "busy", "starting", "stopped", "offline"] {
            assert!(
                refuse_fenced_default("roster-a", state, false).is_ok(),
                "{state}"
            );
        }
    }

    #[test]
    fn cad2746_spawn_agent_instructions_carries_profile_id_when_present() {
        assert_eq!(
            spawn_agent_instructions(Some("ap_123")).as_deref(),
            Some("Hosted agent profile: ap_123")
        );
        assert_eq!(spawn_agent_instructions(None), None);
    }

    #[test]
    fn cad2746_activity_snapshot_tracks_issue_and_clears_on_terminal() {
        let mut lanes = BTreeMap::new();
        record_hosted_kickoff(&mut lanes, "seat-a", "hla_seat", "CAD-2746");
        lanes.insert(
            "hla_spawn".to_string(),
            ActiveHostedLane {
                alias: "omp-123".to_string(),
                issue_id: String::new(),
                spawned: true,
            },
        );
        record_hosted_kickoff(&mut lanes, "omp-123", "hla_spawn", "CAD-2747");

        assert_eq!(
            active_issue_for_alias(&lanes, "seat-a").as_deref(),
            Some("CAD-2746")
        );
        assert_eq!(
            active_issue_for_alias(&lanes, "omp-123").as_deref(),
            Some("CAD-2747")
        );
        assert_eq!(lanes.values().filter(|l| l.spawned).count(), 1);

        let busy = build_agent_activity_snapshot(
            Some("busy"),
            active_issue_for_alias(&lanes, "seat-a"),
            "2026-11-24T08:00:00Z",
        );
        assert_eq!(busy.status, "busy");
        assert_eq!(busy.issue_id.as_deref(), Some("CAD-2746"));
        assert_eq!(busy.summary.as_deref(), Some("Working on CAD-2746"));

        lanes.remove("hla_seat");
        let idle = build_agent_activity_snapshot(
            Some("idle"),
            active_issue_for_alias(&lanes, "seat-a"),
            "2026-11-24T08:05:00Z",
        );
        assert_eq!(idle.status, "idle");
        assert_eq!(idle.issue_id, None);
        assert_eq!(idle.summary.as_deref(), Some("Idle"));
    }

    #[test]
    fn cad2746_reserve_spawn_slot_enforces_cap_and_releases() {
        let mut lanes = BTreeMap::new();
        assert!(reserve_spawn_slot(&mut lanes, "hla_1", 1).is_ok());
        // Duplicate retry on same assignment stays idempotent; second distinct assignment is refused.
        assert!(reserve_spawn_slot(&mut lanes, "hla_1", 1).is_ok());
        let err = reserve_spawn_slot(&mut lanes, "hla_2", 1).unwrap_err();
        assert_eq!(err.code(), Some("runner_at_capacity"));
        lanes.remove("hla_1");
        assert!(reserve_spawn_slot(&mut lanes, "hla_2", 1).is_ok());
    }

    #[test]
    fn cad2777_resume_hosted_worker_enforces_worktree_and_resumes_stopped_worker() {
        let dir = tempfile::Builder::new().prefix("c2777").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let wt_a = dir.path().join("wt-a");
        let wt_b = dir.path().join("wt-b");
        std::fs::create_dir_all(&wt_a).unwrap();
        std::fs::create_dir_all(&wt_b).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd_a = wt_a.to_string_lossy().into_owned();
        shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias: "fake-abc123",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: &cwd_a,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared.store.set_enabled("fake-abc123", false).unwrap();

        let spec = crate::local_assignment::ResumeSpec {
            runner_id: "lri_00000000-0000-4000-8000-000000000001".into(),
            prior_assignment_id: Some("hla_00000000-0000-4000-8000-000000000001".into()),
            lane_alias: Some("fake-abc123".into()),
            target_agent_id: None,
            model: None,
            effort: None,
        };

        // Cross-worktree mismatch fails closed
        let mismatch = shared
            .resume_hosted_worker(&spec, "fake-abc123", &wt_b, false)
            .unwrap_err();
        assert_eq!(mismatch.code(), Some("resume_worktree_mismatch"));

        // Matching worktree invokes try_resume and enables the stopped worker
        let adapter = shared
            .resume_hosted_worker(&spec, "fake-abc123", &wt_a, false)
            .unwrap();
        assert_eq!(adapter, "fake/fake");
        assert!(shared.store.agent("fake-abc123").unwrap().enabled);

        // Shut down spawned actor thread cleanly before tempdir cleanup
        shared
            .closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        shared.wake();
        let ctls: Vec<_> = shared
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .values()
            .cloned()
            .collect();
        shared.stop_ctls(&ctls);
    }
}
