//! CAD-534: `cadence daemon` monitors RPC handlers — moved verbatim from src/daemon.rs; the
//! item→file map is src/daemon/split-map.toml
//! (scripts/split-daemon regenerates it).

use super::*;

impl Shared {
    pub(super) fn rpc_monitor_register(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let tasks = params
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::rejected("Monitor registration requires a tasks array"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::rejected("Monitor task ids must be strings"))
            })
            .collect::<Result<Vec<_>>>()?;
        let owner = required_str(params, "owner")?;
        let interval = optional_u64(params, "interval_secs").unwrap_or(60);
        let dispatch_enabled = params
            .get("dispatch_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let auto_dispatch_enabled = params
            .get("auto_dispatch_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (monitor, duplicate) = self.store.register_monitor(
            required_str(params, "monitor")?,
            required_str(params, "project")?,
            owner,
            interval,
            &tasks,
            dispatch_enabled,
            auto_dispatch_enabled,
        )?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({
            "monitor": monitor.to_json(&coverage, open, total),
            "duplicate": duplicate,
        }))
    }

    pub(super) fn rpc_monitor_list(self: &Arc<Self>) -> Result<Value> {
        let mut monitors = Vec::new();
        for monitor in self.store.monitors()? {
            let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
            monitors.push(monitor.to_json(&coverage, open, total));
        }
        Ok(json!({"monitors": monitors}))
    }

    pub(super) fn rpc_monitor_show(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "monitor")?;
        let (monitor, coverage, open, total) = self.store.monitor_view(id)?;
        Ok(json!({"monitor": monitor.to_json(&coverage, open, total)}))
    }

    pub(super) fn rpc_monitor_heartbeat(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "monitor")?;
        let monitor = self.store.monitor_heartbeat(id)?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({"monitor": monitor.to_json(&coverage, open, total)}))
    }

    pub(super) fn rpc_monitor_alerts(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "monitor")?;
        let after = optional_i64(params, "after").unwrap_or(0);
        let open_only = params.get("open").and_then(Value::as_bool).unwrap_or(false);
        let limit = optional_i64(params, "limit").unwrap_or(100);
        let alerts = self.store.monitor_alerts(id, after, open_only, limit)?;
        let cursor = alerts.last().map(|a| a.seq).unwrap_or(after);
        Ok(json!({
            "monitor": id,
            "alerts": alerts.iter().map(store::MonitorAlert::to_json).collect::<Vec<_>>(),
            "cursor": cursor,
        }))
    }

    pub(super) fn rpc_monitor_alert_ack(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "monitor")?;
        let seq = optional_i64(params, "alert")
            .ok_or_else(|| Error::rejected("Monitor alert acknowledgement requires --alert"))?;
        let by = required_str(params, "by")?;
        let alert = self.store.ack_monitor_alert(id, seq, by)?;
        self.wake();
        Ok(json!({"alert": alert.to_json()}))
    }

    /// `monitor stop` — operator only, by the connection (CAD-373).
    pub(super) fn rpc_monitor_stop(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("monitor stop", params, peer_pid)?;
        let id = required_str(params, "monitor")?;
        let monitor = self.store.stop_monitor(id)?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({"monitor": monitor.to_json(&coverage, open, total)}))
    }

    /// One guarded handoff into the existing job-dispatch transaction. The
    /// public RPC remains an explicit operator action — proven from the
    /// connection (CAD-373), never from a `pane` field; the monitor
    /// watcher may call the same helper only for a separately persisted
    /// automatic opt-in.
    pub(super) fn rpc_monitor_dispatch(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("monitor dispatch", params, peer_pid)?;
        let monitor_id = required_str(params, "monitor")?;
        let task_id = required_str(params, "task")?;
        self.monitor_dispatch_task(monitor_id, task_id, false)
    }

    pub(super) fn monitor_dispatch_task(
        self: &Arc<Self>,
        monitor_id: &str,
        task_id: &str,
        automatic: bool,
    ) -> Result<Value> {
        if automatic {
            self.plan_gate_task(task_id)?;
            // Hold the pending-request mutex across the store transaction.
            // The snapshot contains every alias, while the transaction
            // re-reads the task's current assignee before applying it, so an
            // approval arriving concurrently cannot be missed or bypassed.
            let pending = self.pending.lock().unwrap();
            let pending_aliases: HashSet<String> = pending
                .values()
                .map(|request| request.alias.clone())
                .collect();
            let (task, message, duplicate, behind_dead) =
                self.store.dispatch_automatic_monitor_task(
                    monitor_id,
                    task_id,
                    &pending_aliases,
                    &format!("monitor:{monitor_id}"),
                )?;
            drop(pending);
            let assignee = task.assignee.clone().ok_or_else(|| {
                Error::internal("automatic dispatch returned a task without an assignee")
            })?;
            let _ = self.store.event_public(
                store::Store::DAEMON_STREAM,
                "monitor_dispatch",
                json!({"monitor": monitor_id, "task": task_id,
                       "message": message, "duplicate": duplicate,
                       "queued_behind_dead": behind_dead,
                       "automatic": true}),
            );
            self.notify_agent(&assignee);
            self.wake();
            return Ok(json!({
                "monitor": monitor_id,
                "task": task.to_json(),
                "message": message,
                "duplicate": duplicate,
                "queued_behind_dead": behind_dead,
            }));
        }
        let monitor = self.store.monitor(monitor_id)?;
        if monitor.state != "active" {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' is {} — dispatch requires an active check",
                monitor.state
            )));
        }
        if !monitor.dispatch_enabled {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' has dispatch disabled — enable it explicitly at registration"
            )));
        }
        if !self.store.monitor_is_covered(monitor_id, task_id)? {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is outside monitor '{monitor_id}' coverage"
            )));
        }
        let task = self.store.task(task_id)?;
        let job = self.store.job(&task.job_id)?;
        self.plan_gate_task(task_id)?;
        if job.state != "open" || job.repo.as_deref() != Some(monitor.project.as_str()) {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is not in monitor project '{}' with an open job",
                monitor.project
            )));
        }
        // A retry of a kickoff already claimed by the worker must reuse the
        // existing message atomically, even though the worker is now busy.
        // The duplicate-only store branch never mints a new revision.
        if matches!(task.state.as_str(), "dispatched" | "running") {
            if let Some((task, message, duplicate, behind_dead)) =
                self.store.duplicate_task_dispatch(task_id)?
            {
                self.store.resolve_monitor_dispatch_blocked(
                    monitor_id,
                    task_id,
                    epoch_secs(),
                    "monitor_dispatch",
                )?;
                // The durable job-dispatch row is already the idempotency
                // evidence for an automatic retry. Manual RPC callers keep
                // their historical event for every explicit invocation;
                // the watcher must not emit one event per interval.
                if !automatic {
                    let _ = self.store.event_public(
                        store::Store::DAEMON_STREAM,
                        "monitor_dispatch",
                        json!({"monitor": monitor_id, "task": task_id,
                               "message": message, "duplicate": duplicate,
                               "queued_behind_dead": behind_dead,
                               "automatic": false}),
                    );
                }
                self.wake();
                return Ok(json!({
                    "monitor": monitor_id,
                    "task": task.to_json(),
                    "message": message,
                    "duplicate": duplicate,
                    "queued_behind_dead": behind_dead,
                }));
            }
            return Err(Error::rejected(format!(
                "Task '{task_id}' has no live kickoff — only draft or revising tasks are eligible"
            )));
        }
        if !matches!(task.state.as_str(), "draft" | "revising") {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is '{}' — only draft or revising tasks are eligible",
                task.state
            )));
        }
        if task
            .acceptance
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err(Error::rejected(format!(
                "Task '{task_id}' has no acceptance criteria — dispatch is refused"
            )));
        }
        let assignee = task
            .assignee
            .as_deref()
            .ok_or_else(|| Error::rejected(format!("Task '{task_id}' has no explicit assignee")))?;
        let agent = self.store.agent(assignee)?;
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is a mailbox, not a dispatchable worker"
            )));
        }
        // The fake provider is an in-process fixture and deliberately has no
        // transport endpoint. Every real actor publishes one when open.
        let live_endpoint =
            agent.endpoint.is_some() || (agent.provider == "fake" && agent.endpoint_kind == "fake");
        if !agent.enabled || !live_endpoint || agent.state != "idle" {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is not demonstrably idle and live (state {}, endpoint {})",
                agent.state, live_endpoint
            )));
        }
        if registry::ready_gate(&agent.provider, &agent.endpoint_kind)
            && agent
                .params
                .as_ref()
                .and_then(|p| p.get("auto_ready"))
                .and_then(Value::as_str)
                != Some("verified")
        {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' requires an explicit readiness claim; automatic dispatch is refused"
            )));
        }
        if self
            .pending
            .lock()
            .unwrap()
            .values()
            .any(|request| request.alias == assignee)
        {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is waiting on an approval request"
            )));
        }
        if self.store.queued_count(assignee)? > 0 {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' has queued work; dispatch is refused"
            )));
        }
        let unfinished = self.store.tasks_for_assignee(assignee)?;
        if unfinished.iter().any(|other| other.id != task_id) {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' already has unfinished task work"
            )));
        }
        let (task, message, duplicate, behind_dead) =
            self.store
                .dispatch_task(task_id, None, None, &format!("monitor:{monitor_id}"))?;
        self.store.resolve_monitor_dispatch_blocked(
            monitor_id,
            task_id,
            epoch_secs(),
            "monitor_dispatch",
        )?;
        let _ = self.store.event_public(
            store::Store::DAEMON_STREAM,
            "monitor_dispatch",
            json!({"monitor": monitor_id, "task": task_id,
                   "message": message, "duplicate": duplicate,
                   "queued_behind_dead": behind_dead,
                   "automatic": automatic}),
        );
        self.notify_agent(assignee);
        self.wake();
        Ok(json!({
            "monitor": monitor_id,
            "task": task.to_json(),
            "message": message,
            "duplicate": duplicate,
            "queued_behind_dead": behind_dead,
        }))
    }
}
