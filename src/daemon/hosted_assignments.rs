//! Explicit operator configuration and daemon-owned workspace event workers.
use super::*;
use crate::local_assignment::{events, reporting::ReportDriver, SpawnSpec};
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
        let cwd = std::fs::canonicalize(worktree)
            .map_err(|_| Error::rejected("Working directory must exist"))?;
        self.store.register_agent(&crate::store::NewAgent {
            alias: &alias,
            provider,
            endpoint_kind: kind,
            role: "worker",
            cwd: &cwd.to_string_lossy(),
            sandbox: "read-only",
            instructions: None,
            params: params_str.as_deref(),
            team_role: None,
            model_policy: None,
        })?;
        self.launch_actor(&alias)?;
        Ok((alias, format!("{provider}/{kind}")))
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
                        let active_spawns =
                            Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
                                String,
                                String,
                            >::new()));

                        let shared_send = self.clone();
                        let send_stop = stop.clone();
                        let send_binding = binding.clone();
                        let default_alias = binding.default_alias().map(str::to_string);
                        let active_for_send = Arc::clone(&active_spawns);
                        let send = Arc::new(move |params: Value| {
                            let target_alias = params["alias"].as_str();
                            let allowed_alias = target_alias.is_some_and(|alias| {
                                default_alias.as_deref() == Some(alias)
                                    || send_binding.agents().iter().any(|a| a.alias == alias)
                                    || active_for_send
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .values()
                                        .any(|spawned| spawned == alias)
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
                        let active_for_snap = Arc::clone(&active_spawns);
                        let snapshot = Arc::new(move |b: &events::WorkspaceBinding| {
                            let mut agents = std::collections::BTreeMap::new();
                            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                            for item in b.agents() {
                                let status = match shared_snap.store.agent_opt(&item.alias) {
                                    Ok(Some(row)) => match row.state.as_str() {
                                        "busy" | "starting" => "busy",
                                        "attention" => "blocked",
                                        "idle" => "idle",
                                        _ => "offline",
                                    },
                                    _ => "offline",
                                };
                                agents.insert(
                                    item.alias.clone(),
                                    events::AgentActivitySnapshot {
                                        status: status.to_string(),
                                        summary: None,
                                        issue_id: None,
                                        updated_at: Some(now.clone()),
                                    },
                                );
                            }
                            let active_spawn_lanes = active_for_snap
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .len() as u32;
                            events::RunnerRuntimeSnapshot {
                                agents,
                                active_spawn_lanes,
                            }
                        });

                        let shared_spawn = self.clone();
                        let active_for_spawn = Arc::clone(&active_spawns);
                        let spawn_worker = Arc::new(
                            move |spec: &SpawnSpec, worktree: &Path, assignment_id: &str| {
                                let (alias, adapter) =
                                    shared_spawn.spawn_hosted_worker(spec, worktree)?;
                                active_for_spawn
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(assignment_id.to_string(), alias.clone());
                                Ok((alias, adapter))
                            },
                        );

                        let active_for_term = Arc::clone(&active_spawns);
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

#[cfg(test)]
mod tests {
    use super::refuse_fenced_default;

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
}
