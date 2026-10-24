//! CAD-534: `cadence daemon` master session RPC handlers — moved verbatim from src/daemon.rs.

use super::*;
use cadence_core::board_api::{
    AppOrigin, MasterContext, MasterQueued, MasterSession, MasterState, MasterTurn,
};

impl Shared {
    /// CAD-1700: `master_stamp` — the board's `master` push frame reads
    /// this cheaply (indexed SQL, no provider call). It changes whenever
    /// the master's running/queued turns, any master thread entry, or the
    /// app assistant / master permission revision (`master_rev`) changes.
    pub(super) fn rpc_master_stamp(&self) -> Result<Value> {
        let alias = crate::master::ALIAS;
        let (live_seq, running, queued, entries, transition_ms) = self.store.master_stamp(alias)?;
        Ok(json!({
            "live": live_seq,
            "running": running,
            "queued": queued,
            "entries": entries,
            "transition_ms": transition_ms,
            "rev": self.master_rev.load(std::sync::atomic::Ordering::Acquire),
        }))
    }

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
        let context = stats
            .as_ref()
            .and_then(|s| s.get("contextUsage"))
            .map(|u| MasterContext {
                tokens: u.get("tokens").cloned().unwrap_or(Value::Null),
                window: u.get("contextWindow").cloned().unwrap_or(Value::Null),
                percent: u.get("percent").cloned().unwrap_or(Value::Null),
            });
        let session_json = session.as_ref().map(|s| MasterSession {
            id: s.get("sessionId").cloned().unwrap_or(Value::Null),
            messages: s.get("messageCount").cloned().unwrap_or(Value::Null),
            streaming: s.get("isStreaming").cloned().unwrap_or(Value::Null),
            compacting: s.get("isCompacting").cloned().unwrap_or(Value::Null),
        });
        // The turn the working row mirrors: `running` first, else the
        // queue head. `since` is epoch seconds — `started` while it
        // runs, `created` while it waits.
        // CAD-1098: the conversation the turn belongs to, so a panel can
        // say "queued" while another conversation's turn runs.
        let conversation_of = |id: &str| -> Option<String> {
            self.store
                .message_conversation(alias, id)
                .ok()
                .flatten()
                .map(|t| t.id)
        };
        let running = self.store.running_message(alias)?;
        // CAD-1700: the complete ordered queue behind the board's master
        // pane, as routing metadata only — never message bodies, never a
        // 20-row page. `queued_metadata` uses `queued_head`'s delivery
        // order, so the head is position 1.
        let queued = self.store.queued_metadata(alias)?;
        // Only a queued turn needs the head's body and start time; one row
        // read, never the whole queue's.
        let head = match &running {
            Some(_) => None,
            None => self.store.queued_head(alias)?,
        };
        let turn = match &running {
            Some(m) => Some(MasterTurn {
                state: "working".into(),
                message: m.id.clone(),
                conversation: conversation_of(&m.id),
                app_origin: self.master_turn_app_origin(&m.id),
                summary: Some(m.body.clone()),
                since: m.started.unwrap_or(m.created),
                position: None,
            }),
            None => head.as_ref().map(|m| {
                let conversation = match queued.iter().find(|(id, _)| id == &m.id) {
                    Some((_, thread)) => thread.clone(),
                    // The head left the queue between the two reads:
                    // report its own conversation, never a neighbour's.
                    None => conversation_of(&m.id),
                };
                MasterTurn {
                    state: "queued".into(),
                    message: m.id.clone(),
                    conversation,
                    app_origin: self.master_turn_app_origin(&m.id),
                    summary: Some(m.body.clone()),
                    since: m.created,
                    position: Some(1),
                }
            }),
        };
        let queue = queued
            .iter()
            .map(|(id, conversation)| MasterQueued {
                message: id.clone(),
                conversation: conversation.clone(),
            })
            .collect::<Vec<_>>();
        Ok(serde_json::to_value(MasterState {
            alias: alias.to_string(),
            provider: agent.provider.clone(),
            endpoint_kind: agent.endpoint_kind.clone(),
            live,
            model: model_id,
            model_label: model
                .as_ref()
                .and_then(|m| m.get("name").or_else(|| m.get("id")))
                .cloned()
                .unwrap_or(Value::Null),
            effort,
            context,
            session: session_json,
            commands: commands.iter().map(|c| (*c).to_string()).collect(),
            turn,
            queue,
            queued: self.store.queued_count(alias)?,
        })
        .expect("master state serializes"))
    }

    /// Best-effort navigation metadata for the exact persisted master turn.
    /// This projection proves the current app and conversation while the
    /// runtime snapshot is held; it is not an authorization or revocation-atomic grant.
    fn master_turn_app_origin(&self, message_id: &str) -> Option<AppOrigin> {
        let alias = crate::master::ALIAS;
        let thread = self
            .store
            .message_conversation(alias, message_id)
            .ok()
            .flatten()?;
        let install = thread
            .install_id
            .as_deref()
            .filter(|install| !install.is_empty())?;
        if thread.alias != alias
            || thread.archived
            || thread.runs_as_home()
            || thread.id.is_empty()
            || thread.subject.is_some()
        {
            return None;
        }

        let pm_dir = self.pm_dir().ok()?;
        let pm = self.pm_at(&pm_dir).ok()?;
        let conversation_id = thread.id.clone();
        crate::issue::app_catalog::workspace::with_runtime_read(&pm, install, |row, _files| {
            let Some(digest) = row.get("digest").and_then(Value::as_str) else {
                return Ok(None);
            };
            if row.get("install_id").and_then(Value::as_str) != Some(install) {
                return Ok(None);
            }
            let status = self.store.app_capability_status(install, digest)?;
            if status.get("state").and_then(Value::as_str) != Some("approved") {
                return Ok(None);
            }

            // Re-read both edges under the held runtime snapshot. The
            // first lookup binds this exact message to the same live
            // master conversation. Subject/campaign conversations were
            // refused above: this selector reaches RecordStore::open for
            // those, which may initialize or migrate persistent storage.
            let current_turn = match self.store.running_message(alias)? {
                Some(message) => Some(message),
                None => self.store.queued_head(alias)?,
            };
            if current_turn.as_ref().map(|message| message.id.as_str()) != Some(message_id) {
                return Ok(None);
            }
            let Some(current_thread) = self.store.message_conversation(alias, message_id)? else {
                return Ok(None);
            };
            if current_thread.id != conversation_id
                || current_thread.alias != alias
                || current_thread.archived
                || current_thread.runs_as_home()
                || current_thread.subject.is_some()
                || current_thread.install_id.as_deref() != Some(install)
            {
                return Ok(None);
            }
            // Persisted provenance: the enqueue entry's own send-time
            // stamp. A context stamp is re-proved against the live store;
            // an installation-only stamp (an app chat with no context) is
            // a valid destination with an empty context.
            let Some(stamp) = self.store.message_app_stamp(message_id)? else {
                return Ok(None);
            };
            if stamp.get("verified") != Some(&Value::Bool(true))
                || stamp.get("install_id").and_then(Value::as_str) != Some(install)
            {
                return Ok(None);
            }
            let context = if stamp.get("context_id").is_some() {
                let Some(app) = self.store.message_app(message_id)? else {
                    return Ok(None);
                };
                let Some(context) = app
                    .get("context_id")
                    .and_then(Value::as_str)
                    .filter(|context| !context.is_empty())
                else {
                    return Ok(None);
                };
                if app.get("install_id").and_then(Value::as_str) != Some(install)
                    || (current_thread.context_id.as_deref() != Some(context)
                        && !(current_thread.is_general && current_thread.context_id.is_none()))
                {
                    return Ok(None);
                }
                self.store.app_context_proof(install, context)?;
                context.to_string()
            } else {
                String::new()
            };
            let app = json!({
                "install_id": install,
                "context_id": (!context.is_empty()).then_some(context.as_str()),
            });
            self.verify_conversation_selector(alias, &app, &conversation_id)?;
            Ok(Some(AppOrigin {
                install_id: install.to_string(),
                context_id: context,
                conversation_id: conversation_id.clone(),
            }))
        })
        .ok()
        .flatten()
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
        let policy = self.stored_pi_policy()?;
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
            if crate::pi_policy::require_allowed(policy.as_ref(), "master", id).is_ok() {
                roles.push("master");
            }
            if crate::pi_policy::require_allowed(policy.as_ref(), "worker", id).is_ok() {
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
        // CAD-1700: `message` scopes `stop` and `cancel` to one master turn.
        // It is refused on every other verb, so it cannot widen one. Each
        // refusal leaves a `master_command` event, like every other call.
        let message = match params.get("message") {
            None => None,
            Some(value) => {
                if command != "stop" && command != "cancel" {
                    let error =
                        format!("'/{command}' takes no message - only /stop and /cancel do");
                    audit("refused", Some(&error));
                    return Err(Error::invalid("message_not_allowed", error));
                }
                match value.as_str() {
                    Some(id) if !id.is_empty() && id.chars().count() <= 128 => Some(id),
                    _ => {
                        let error = "message must be a non-empty string of at most 128 characters";
                        audit("refused", Some(error));
                        return Err(Error::invalid("message_invalid", error));
                    }
                }
            }
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
            // bounds how long the answer waits for the settle. With a
            // `message`, the stop is bound to that exact running turn (CAD-1700).
            let mut p = json!({"alias": alias});
            if let Some(wait) = params.get("wait") {
                p["wait"] = wait.clone();
            }
            let out = match message {
                Some(message) => {
                    let running = self.store.running_message(alias);
                    if running
                        .as_ref()
                        .ok()
                        .and_then(|m| m.as_ref())
                        .map(|m| m.id.as_str())
                        != Some(message)
                    {
                        let error = format!(
                            "message {message} is not the master's running turn — nothing was stopped"
                        );
                        audit("refused", Some(&error));
                        return Err(Error::invalid("turn_not_running", error));
                    }
                    self.rpc_interrupt_bound(&p, peer_pid, Some(message))
                }
                None => self.rpc_interrupt(&p, peer_pid),
            };
            match &out {
                Ok(_) => audit("ok", None),
                Err(e) => audit("refused", Some(&e.to_string())),
            }
            return out.map(|v| json!({"command": command, "ok": true, "result": v}));
        }
        if command == "cancel" {
            // CAD-1700: cancel exactly one still-queued operator message to the
            // master. A running or submitting turn is never cancelled here.
            let Some(id) = message else {
                audit("refused", Some("message_required"));
                return Err(Error::invalid(
                    "message_required",
                    "cancel needs the queued message id - `message` is required",
                ));
            };
            let refuse = |code: &'static str, text: String| {
                audit("refused", Some(&text));
                Err(Error::invalid(code, text))
            };
            let found = match self.store.message(id)? {
                // CAD-1700: the source string alone is caller-supplied, so
                // it is not provenance. The store's canonical sender label is
                // the proof: only `Sender::OperatorChat` records `operator`;
                // an agent records its alias (with a uid), a member or CLI
                // principal its own label.
                Some(m)
                    if m.alias == alias
                        && m.source == "operator"
                        && m.sender.as_deref() == Some("operator")
                        // An agent may literally be named `operator`; the
                        // canonical uid is what tells it from the operator's
                        // own chat (only the agent row has one).
                        && m.sender_uid.is_none()
                        && m.reply_to.is_none() =>
                {
                    m
                }
                _ => {
                    return refuse(
                        "message_not_cancellable",
                        format!("message {id} is not an operator message queued to the master"),
                    )
                }
            };
            match found.state.as_str() {
                "queued" => {}
                "running" | "submitting" => {
                    return refuse(
                        "message_running",
                        format!("message {id} is already running - use Stop"),
                    )
                }
                other => {
                    return refuse(
                        "message_not_queued",
                        format!(
                            "message {id} is '{other}' - only a queued message can be cancelled"
                        ),
                    )
                }
            }
            let cancelled = self
                .store
                .cancel(id, "operator", Some("cancelled from the board"))?;
            if let Some(result) = cancelled.result.clone() {
                self.notify_routed_target(&cancelled, &result);
            }
            audit("ok", None);
            self.wake();
            return Ok(json!({"command": command, "ok": true,
                             "result": {"message": cancelled.to_json()}}));
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
    "state", "stats", "models", "model", "levels", "effort", "compact", "new", "stop", "cancel",
    "help",
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

#[cfg(test)]
mod cad1700_projection_tests {
    use super::*;
    use crate::store::NewAgent;
    use crate::test_seam::{scoped, Asserted};

    fn fixture() -> (tempfile::TempDir, Arc<Shared>) {
        let dir = tempfile::tempdir().unwrap();
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("no-pm").to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "master",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        (dir, shared)
    }

    /// The master pane looks for its own queued message and what is ahead
    /// of it in `master_state.queue`. The queue is complete in exact
    /// delivery order, so the 21st queued message and its predecessor are
    /// still represented, and the head row keeps its turn semantics
    /// (state, message, summary, since, position). Red before CAD-1700 P2:
    /// the projection stopped at 20 rows.
    #[test]
    fn cad1700_master_state_queue_is_complete_past_twenty() {
        let (_dir, shared) = fixture();
        let pid = std::process::id();
        // Send admission (launch/model gates) is not under test: seed the
        // canonical rows a Home send produces. `enqueue` itself writes the
        // operator thread note when the alias has a home thread (the same
        // `thread_note_enqueued` path every queued message takes), and that
        // note carries the message id, so each row resolves its home
        // conversation.
        shared.store.ensure_thread("master").unwrap();
        for i in 1..=25 {
            let id = format!("m{i}");
            shared
                .store
                .enqueue("master", &format!("hi {id}"), None, &id, "operator")
                .unwrap();
        }
        let state = scoped(Asserted::Operator, || {
            shared.dispatch("master_state", &json!({}), pid)
        })
        .unwrap();
        let queue = state["queue"].as_array().unwrap();
        let ids = queue
            .iter()
            .map(|row| row["message"].as_str().unwrap())
            .collect::<Vec<_>>();
        let expected = (1..=25).map(|i| format!("m{i}")).collect::<Vec<_>>();
        assert_eq!(
            ids,
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "the queue is complete and in delivery order"
        );
        assert_eq!(state["queued"], json!(25));
        assert!(ids.contains(&"m20"), "the 20th message is represented");
        assert!(ids.contains(&"m21"), "the 21st message is represented");
        assert_eq!(state["turn"]["state"], json!("queued"));
        assert_eq!(state["turn"]["message"], json!("m1"));
        assert_eq!(state["turn"]["summary"], json!("hi m1"));
        assert_eq!(state["turn"]["position"], json!(1));
        let home = shared.store.thread("master").unwrap().unwrap().id;
        for row in queue {
            assert_eq!(
                row["conversation"],
                json!(home),
                "each row's conversation comes from the one projection"
            );
        }
    }
}
