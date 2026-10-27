//! Explicit operator configuration and daemon-owned workspace event workers.
use super::*;
use crate::local_assignment::{events, reporting::ReportDriver};
use crate::remote_cli::RemoteTarget;

impl Shared {
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
                events::connect(
                    &self.state_dir,
                    &target,
                    &agent,
                    fyi_only,
                    optional_str(params, "name"),
                    optional_str(params, "repo").map(Path::new),
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
                        let shared = self.clone();
                        let send_stop = stop.clone();
                        let send_binding = binding.clone();
                        let default_alias = binding.default_alias().map(str::to_string);
                        let send = Arc::new(move |params: Value| {
                            if send_stop.load(Ordering::SeqCst)
                                || shared.closing.load(Ordering::SeqCst)
                                || shared.execution_mode.fenced()
                                || params["alias"].as_str() != default_alias.as_deref()
                            {
                                return Err(Error::busy_coded(
                                    "binding_cancelled",
                                    "hosted assignment binding is no longer active",
                                ));
                            }
                            // Validate the persisted selection again immediately before send.
                            if !events::bindings(&shared.state_dir)?
                                .iter()
                                .any(|current| current == &send_binding)
                            {
                                return Err(Error::busy_coded(
                                    "binding_cancelled",
                                    "hosted workspace was disconnected or rebound",
                                ));
                            }
                            // Existing agent_send semantics, without operator or steering authority.
                            shared.send_as(&params, &|_| Ok(store::Sender::Unattributed))
                        });
                        let notifier = self.clone();
                        let notify = Arc::new(move || notifier.wake());
                        thread::spawn(move || events::run(state, binding, stop, send, notify));
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
}
