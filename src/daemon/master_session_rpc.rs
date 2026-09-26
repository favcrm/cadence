//! CAD-534: `cadence daemon` master session RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

impl Shared {
    /// `master_state` (CAD-551) — what the board's master header shows:
    /// the provider, the live session's model/effort/context (when the
    /// provider answers — Pi's `get_state`/`get_session_stats`), the
    /// verbs its session commands accept, and the turn in flight or
    /// queued. Read-only like `agent_show`; a master without a live
    /// endpoint still answers with its stored fields.
    pub(super) fn rpc_master_state(&self, _params: &Value) -> Result<Value> {
        let alias = crate::master::ALIAS;
        let agent = self.store.agent(alias).map_err(|_| {
            Error::rejected("the master is not started — `cadence master start` registers it")
        })?;
        let adapter = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .get(alias)
            .and_then(|ctl| ctl.adapter.lock().unwrap().clone());
        let commands = adapter
            .as_ref()
            .map(|a| a.session_commands())
            .unwrap_or(&[]);
        // Live session fields win over the stored launch params when the
        // provider answers `state`/`stats` (Pi does; a stopped or
        // answering-less provider leaves the stored values standing).
        let live = adapter.is_some();
        let session = commands
            .contains(&"state")
            .then(|| {
                adapter
                    .as_ref()
                    .and_then(|a| a.session_command("state", None).ok())
            })
            .flatten();
        let stats = commands
            .contains(&"stats")
            .then(|| {
                adapter
                    .as_ref()
                    .and_then(|a| a.session_command("stats", None).ok())
            })
            .flatten();
        let model = session
            .as_ref()
            .and_then(|s| s.get("model"))
            .cloned()
            .filter(|m| !m.is_null())
            .map(|m| {
                json!({
                    "id": m.get("id").or_else(|| m.get("name")).cloned().unwrap_or(Value::Null),
                    "name": m.get("name").cloned().unwrap_or(Value::Null),
                    "provider": m.get("provider").cloned().unwrap_or(Value::Null),
                })
            });
        let configured = |key: &str| {
            agent
                .params
                .as_ref()
                .and_then(|p| p.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let model_id = model
            .as_ref()
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.model.clone())
            .or_else(|| configured("model"));
        let effort = session
            .as_ref()
            .and_then(|s| s.get("thinkingLevel"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.effort.clone())
            .or_else(|| configured("effort"));
        let context = stats.as_ref().and_then(|s| s.get("contextUsage")).map(|u| {
            json!({
                "tokens": u.get("tokens").cloned().unwrap_or(Value::Null),
                "window": u.get("contextWindow").cloned().unwrap_or(Value::Null),
                "percent": u.get("percent").cloned().unwrap_or(Value::Null),
            })
        });
        let session_json = session.as_ref().map(|s| {
            json!({
                "id": s.get("sessionId").cloned().unwrap_or(Value::Null),
                "messages": s.get("messageCount").cloned().unwrap_or(Value::Null),
                "streaming": s.get("isStreaming").cloned().unwrap_or(Value::Null),
                "compacting": s.get("isCompacting").cloned().unwrap_or(Value::Null),
            })
        });
        // The turn the working row mirrors: `running` first, else the
        // queue head. `since` is epoch seconds — `started` while it
        // runs, `created` while it waits.
        let running = self.store.running_message(alias)?;
        let turn = match &running {
            Some(m) => json!({
                "state": "working",
                "message": m.id,
                "summary": m.body,
                "since": m.started.unwrap_or(m.created),
            }),
            None => match self.store.queued_head(alias)? {
                Some(m) => json!({
                    "state": "queued",
                    "message": m.id,
                    "summary": m.body,
                    "since": m.created,
                }),
                None => Value::Null,
            },
        };
        Ok(json!({
            "alias": alias,
            "provider": agent.provider,
            "endpoint_kind": agent.endpoint_kind,
            "live": live,
            "model": model_id,
            "model_label": model.as_ref().and_then(|m| m.get("name").or_else(|| m.get("id"))).cloned().unwrap_or(Value::Null),
            "effort": effort,
            "context": context,
            "session": session_json,
            "commands": commands,
            "turn": turn,
            "queued": self.store.queued_count(alias)?,
        }))
    }

    /// `master_models` (CAD-575) — the operator's per-role read of the
    /// master's model vocabulary, backing the board's model picker:
    /// the live session's model/effort, every model pi offers (plus
    /// the policy-listed ids it did not offer) annotated with which
    /// roles may run it (`allowed_for`), the Devin cost tier, and the
    /// session's thinking-effort levels. Operator-only by connection;
    /// the verb takes no parameters at all — an identity-shaped field
    /// is refused by [`Self::operator_connection`], anything else here,
    /// so a read that grows knobs later never inherits silent
    /// authority.
    ///
    /// It degrades, never blocks: a stopped (or never registered)
    /// master answers from its stored row with `efforts` empty and
    /// `models` drawn from the policy lists alone; the Devin catalog
    /// comes from the pi-devin cache file or a bounded, memoized
    /// `devin models list` — every miss is `cost_tier: "unknown"`.
    pub(super) fn rpc_master_models(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("master models", params, peer_pid)?;
        if let Some(extra) = params.as_object().and_then(|o| o.keys().next()) {
            return Err(Error::rejected(format!(
                "master models takes no parameters — '{extra}' is refused (CAD-575)"
            )));
        }
        let alias = crate::master::ALIAS;
        let agent = self.store.agent(alias).ok();
        let adapter = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .get(alias)
            .and_then(|ctl| ctl.adapter.lock().unwrap().clone());
        let live = adapter.is_some();
        let commands = adapter
            .as_ref()
            .map(|a| a.session_commands())
            .unwrap_or(&[]);
        let ask = |command: &str| {
            commands
                .contains(&command)
                .then(|| {
                    adapter
                        .as_ref()
                        .and_then(|a| a.session_command(command, None).ok())
                })
                .flatten()
        };
        let session = ask("state");
        let offered = ask("models");
        let levels = ask("levels");
        // The policy's per-role vocabularies annotate every offered
        // model and supply the rows the provider never offered — a
        // missing `[pi]` is two empty lists, like the launch gates.
        let policy = self
            .pm_dir()
            .ok()
            .map(|dir| crate::pi_policy::read(&dir))
            .transpose()?
            .flatten();
        let (master_list, worker_list) = policy
            .as_ref()
            .map(|p| {
                (
                    p.models.allow_for("master").to_vec(),
                    p.models.allow_for("worker").to_vec(),
                )
            })
            .unwrap_or_default();
        let allowed_for = |id: &str| -> Vec<&'static str> {
            let mut roles = Vec::new();
            if master_list.iter().any(|m| m == id) {
                roles.push("master");
            }
            if worker_list.iter().any(|m| m == id) {
                roles.push("worker");
            }
            roles
        };
        // The session's own list first — it is what the master could
        // switch to live — then the policy-listed ids the provider did
        // not offer; the allowlist stays the vocabulary the gate
        // enforces even when the provider's list is shorter.
        let mut order: Vec<String> = Vec::new();
        let mut labels: HashMap<String, String> = HashMap::new();
        for m in offered
            .as_ref()
            .and_then(|o| o.get("models"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = m.get("id").and_then(Value::as_str) else {
                continue;
            };
            let provider = m
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let full = if provider.is_empty() {
                id.to_string()
            } else {
                format!("{provider}/{id}")
            };
            if !order.contains(&full) {
                order.push(full.clone());
            }
            if let Some(name) = m.get("name").and_then(Value::as_str) {
                labels.entry(full).or_insert_with(|| name.to_string());
            }
        }
        for id in master_list.iter().chain(worker_list.iter()) {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        let catalog = self.devin_catalog.catalog(&self.provider_env);
        let entries: Vec<Value> = order
            .iter()
            .map(|id| {
                let (provider, uid) = id.split_once('/').unwrap_or(("", id.as_str()));
                let (cost_tier, catalog_label) = match provider {
                    "devin" => catalog
                        .as_ref()
                        .and_then(|c| c.lookup(uid))
                        .map(|(tier, label)| (tier.clone(), label.clone()))
                        .unwrap_or(("unknown".to_string(), None)),
                    "openrouter" => ("Paid".to_string(), None),
                    _ => ("unknown".to_string(), None),
                };
                json!({
                    "id": id,
                    "label": labels
                        .get(id)
                        .cloned()
                        .or(catalog_label)
                        .unwrap_or_else(|| id.clone()),
                    "cost_tier": cost_tier,
                    "allowed_for": allowed_for(id),
                })
            })
            .collect();
        // Live session fields win over the stored launch params, the
        // same precedence `master_state` uses — but `model` here is the
        // full `provider/id` the allowlists speak.
        let live_model = session.as_ref().and_then(|s| s.get("model")).and_then(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("name"))
                .and_then(Value::as_str)?;
            let provider = m.get("provider").and_then(Value::as_str);
            Some(match provider {
                Some(p) if !p.is_empty() => format!("{p}/{id}"),
                _ => id.to_string(),
            })
        });
        let configured = |key: &str| {
            agent
                .as_ref()
                .and_then(|a| a.params.as_ref())
                .and_then(|p| p.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let current_model = live_model
            .or_else(|| agent.as_ref().and_then(|a| a.model.clone()))
            .or_else(|| configured("model"));
        let effort = session
            .as_ref()
            .and_then(|s| s.get("thinkingLevel"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.as_ref().and_then(|a| a.effort.clone()))
            .or_else(|| configured("effort"));
        let efforts: Vec<Value> = levels
            .as_ref()
            .and_then(|l| l.get("levels"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(json!({
            "current": {"model": current_model, "effort": effort},
            "models": entries,
            "efforts": efforts,
            "live": live,
        }))
    }

    /// `master_command` (CAD-551) — the operator's provider-session
    /// commands for the master, behind the board's slash menu and its
    /// Stop control. The daemon owns the verb set
    /// ([`MASTER_COMMANDS`]): `stop` is the ordinary `interrupt` path,
    /// every other verb maps through the adapter's declared
    /// [`ProviderAdapter::session_commands`] — never a raw provider
    /// command passthrough. Operator-only by connection; identity
    /// fields are refused. Every call — refused included — leaves a
    /// `master_command` event; a mutation also writes a durable
    /// `system` line on the master's thread.
    pub(super) fn rpc_master_command(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("master command", params, peer_pid)?;
        let command = required_str(params, "command")?;
        let arg = optional_str(params, "arg")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let alias = crate::master::ALIAS;
        let audit = |outcome: &str, error: Option<&str>| {
            let mut payload = json!({"command": command, "by": "operator", "by_kind": "operator",
                                     "outcome": outcome});
            if let Some(arg) = arg {
                payload["arg"] = json!(arg);
            }
            if let Some(error) = error {
                payload["error"] = json!(error);
            }
            let _ = self.store.event_public(alias, "master_command", payload);
        };
        if command == "help" {
            audit("ok", None);
            return Ok(json!({"command": command, "ok": true,
                             "result": {"commands": MASTER_COMMANDS}}));
        }
        if !MASTER_COMMANDS.contains(&command) {
            audit("refused", Some("unknown command"));
            return Err(Error::invalid(
                "unknown_command",
                format!(
                    "unknown master command '/{command}' — the board allows: {}",
                    MASTER_COMMANDS
                        .iter()
                        .map(|c| format!("/{c}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            ));
        }
        if command == "stop" {
            // The daemon's interrupt path: same authorization, audit and
            // stale-turn protection `cadence interrupt` has. `wait`
            // bounds how long the answer waits for the settle.
            let mut p = json!({"alias": alias});
            if let Some(wait) = params.get("wait") {
                p["wait"] = wait.clone();
            }
            let out = self.rpc_interrupt(&p, peer_pid);
            match &out {
                Ok(_) => audit("ok", None),
                Err(e) => audit("refused", Some(&e.to_string())),
            }
            return out.map(|v| json!({"command": command, "ok": true, "result": v}));
        }
        let agent = match self.store.agent(alias) {
            Ok(a) => a,
            Err(_) => {
                audit("refused", Some("the master is not started"));
                return Err(Error::rejected(
                    "the master is not started — no agent 'master'",
                ));
            }
        };
        let adapter = match self.adapter_for(alias) {
            Ok(a) => a,
            Err(e) => {
                audit("refused", Some(&e.to_string()));
                return Err(e);
            }
        };
        if !adapter.session_commands().contains(&command) {
            let error = format!(
                "the '/{command}' command is not supported by {}",
                agent.provider
            );
            audit("refused", Some(&error));
            return Err(Error::rejected(error));
        }
        match adapter.session_command(command, arg) {
            Ok(result) => {
                audit("ok", None);
                if command == "model" {
                    // CAD-559: the verified switch is the launch model
                    // from now on — the next open's re-check must see it
                    // in params.model, and the reported column agrees.
                    if let Some(model) = result.get("requested").and_then(Value::as_str) {
                        if let Err(e) = self.store.set_params(alias, &json!({"model": model})) {
                            eprintln!("master_command params.model for '{alias}' failed: {e}");
                        }
                        if let Err(e) = self.store.set_model_reported(alias, model) {
                            eprintln!("master_command model report for '{alias}' failed: {e}");
                        }
                    }
                }
                if let Some(line) = master_command_line(command, &result) {
                    if let Err(e) = self.store.thread_append(
                        alias,
                        store::NewEntry {
                            role: store::ROLE_SYSTEM,
                            kind: store::KIND_MESSAGE,
                            text: &line,
                            payload: Some(json!({
                                "command": format!("/{command}"),
                                "arg": arg,
                            })),
                            message_id: None,
                        },
                    ) {
                        eprintln!("master_command thread line for '{alias}' failed: {e}");
                    }
                }
                self.wake();
                Ok(json!({"command": command, "ok": true, "result": result}))
            }
            Err(e) => {
                audit("refused", Some(&e.to_string()));
                Err(e)
            }
        }
    }
}

/// The `master_command` verbs (CAD-551) — the only session commands the
/// operator can run against the master, named in the daemon so a relay
/// (the board's `/api/master/command`) can never widen them. `stop`
/// runs the `interrupt` path; `help` answers the list itself; the rest
/// are adapter verbs.
const MASTER_COMMANDS: &[&str] = &[
    "state", "stats", "models", "model", "levels", "effort", "compact", "new", "stop", "help",
];

/// The one-line `system` thread note a `master_command` mutation
/// leaves — the durable "who changed what" beside the `master_command`
/// audit event. Queries write nothing to the thread.
fn master_command_line(command: &str, result: &Value) -> Option<String> {
    match command {
        "model" => {
            let model = result
                .pointer("/model/id")
                .or_else(|| result.pointer("/model/name"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let was = result.get("was").and_then(Value::as_str);
            Some(match was {
                Some(old) => format!("Model → {model} (was {old})"),
                None => format!("Model → {model}"),
            })
        }
        "effort" => result
            .get("level")
            .and_then(Value::as_str)
            .map(|level| format!("Effort → {level}")),
        "compact" => Some("Session context compacted".to_string()),
        "new" => Some("Provider session restarted".to_string()),
        _ => None,
    }
}
