//! CAD-534: `cadence daemon` agents RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

impl Shared {
    pub(super) fn rpc_register(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        crate::agent_settings::refuse_profile_alias(&self.store, alias)?;
        let provider = required_str(params, "provider")?;
        let endpoint =
            optional_str(params, "endpoint_kind").unwrap_or(registry::DEFAULT_ENDPOINT_KIND);
        let role = optional_str(params, "role").unwrap_or("worker");
        let caller_team_role = optional_text(params, "team_role")?;
        let model_policy = optional_text(params, "model_policy")?;
        let cwd = optional_str(params, "cwd");
        let sandbox = optional_str(params, "sandbox").unwrap_or("read-only");
        let caller_instructions = optional_str(params, "instructions");
        let agent_params = optional_str(params, "params");
        if registry::is_inbox_kind(endpoint) != registry::is_inbox_provider(provider) {
            return Err(Error::rejected(
                "Provider 'inbox' and endpoint kind 'inbox' must be used together",
            ));
        }
        // `cadence inbox ack <alias>` is the consume verb: an inbox
        // literally named 'ack' would be unreachable through the CLI.
        if registry::is_inbox_kind(endpoint) && alias == "ack" {
            return Err(Error::rejected(
                "the alias 'ack' is reserved for `cadence inbox ack` — \
                 pick another name for the inbox",
            ));
        }
        // A mailbox never runs a process — its cwd is bookkeeping only,
        // so direct socket callers may omit it (the CLI defaults cwd).
        let cwd = match (cwd, endpoint) {
            (Some(cwd), _) => std::fs::canonicalize(cwd)
                .map_err(|_| Error::rejected("Working directory must exist"))?,
            (None, e) if !registry::has_actor(provider, e) => self.state_dir.clone(),
            (None, _) => return Err(Error::rejected("Missing 'cwd'")),
        };
        // Enumerated launch params are validated at the door — a bad
        // value rejected here never lands on the agent row to be
        // replayed into a provider argv on every resume.
        let mut parsed = match agent_params {
            Some(raw) => {
                let parsed: Value = serde_json::from_str(raw)
                    .map_err(|_| Error::rejected("'params' must be a JSON object"))?;
                registry::validate_launch_params(provider, endpoint, &parsed)?;
                parsed
            }
            None => Value::Null,
        };
        // The launch workdir (and so a confined agent's grant) is the
        // daemon's to set when it relaunches for a ticket, never a caller's.
        if parsed.get("launch_workdir").is_some() {
            return Err(Error::rejected(
                "params 'launch_workdir' is set by the daemon, not the caller",
            ));
        }
        let mut defaulted = false;
        // CAD-556: `pm.yaml [host] confine_pi_workers` is the pm-level
        // default a bare `join … pi` picks up — an explicit `confine`
        // param (`--confine`/`--no-confine`, or a caller's own params)
        // always wins. A [host] table that cannot be parsed refuses
        // the register rather than silently landing unconfined.
        let requested_profile = optional_str(params, "profile");
        if requested_profile.is_none()
            && ["profile", "profile_digest", "profile_revision"]
                .iter()
                .any(|key| parsed.get(*key).is_some())
        {
            return Err(Error::rejected(
                "profile provenance is assigned from the settings row, not the caller",
            ));
        }
        if requested_profile.is_some() && caller_instructions.is_some() {
            return Err(Error::rejected(
                "profile and instructions are mutually exclusive",
            ));
        }
        let mut profile_job: Option<String> = None;
        let mut profile_team: Option<String> = None;
        for key in ["profile", "profile_revision", "profile_digest"] {
            if parsed.get(key).is_some() {
                return Err(Error::rejected(
                    "profile provenance is taken from the settings row, not caller params",
                ));
            }
        }
        if let Some(profile_id) = requested_profile {
            // Join sends the profile id and, when briefing, FilesAndMessage.
            // It does not put a bootstrap flag in params. Only an instructions
            // file or an explicit skip conflicts with the saved profile body.
            if parsed.get("instructions_file").is_some()
                || parsed.get("instructions").is_some()
                || parsed.get("no_bootstrap").and_then(Value::as_bool) == Some(true)
            {
                return Err(Error::rejected(
                    "a profile launch cannot combine an instructions file or skip bootstrap",
                ));
            }
            let requested_model = parsed
                .get("model")
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty());
            let provider_default = model_policy == Some("provider_default");
            let explicit_model = requested_model.is_some() || provider_default;
            let launched = crate::agent_settings::launch_profile(
                &self.store,
                profile_id,
                provider,
                endpoint,
                requested_model,
                provider_default,
            )?;
            let mut obj = match parsed {
                Value::Object(obj) => obj,
                _ => serde_json::Map::new(),
            };
            obj.insert("profile".to_string(), json!(launched.id));
            obj.insert("profile_revision".to_string(), json!(launched.revision));
            obj.insert("profile_digest".to_string(), json!(launched.digest));
            obj.remove("instructions");
            if !explicit_model
                && launched.model != "inherit"
                && registry::supports_model(provider, endpoint)
            {
                obj.insert("model".to_string(), json!(launched.model));
            }
            parsed = Value::Object(obj);
            profile_job = Some(launched.job);
            profile_team = Some(launched.role);
        }
        let team_role = if let Some(profile_role) = profile_team.as_deref() {
            if !crate::model_defaults::canonical_team_role(profile_role) {
                return Err(Error::rejected(
                    "profile role is a team-role lookup, not runtime authority, and it is not a team role",
                ));
            }
            if let Some(caller) = caller_team_role {
                let normalized = crate::model_defaults::normalize_team_role(caller)?;
                if normalized != profile_role {
                    return Err(Error::rejected(
                        "profile role is the team-role lookup and does not match the requested team role",
                    ));
                }
            }
            Some(profile_role)
        } else {
            caller_team_role
        };
        let instructions = profile_job.as_deref().or(caller_instructions);
        defaulted |= requested_profile.is_some();
        let pm_dir = self.pm_dir().ok().filter(|d| d.is_dir());
        // The host default `confine_pi_workers` is the Pi adapter's policy, so
        // every managed Pi-family worker (pi, omp) obeys it — a family
        // registration must never launch unconfined by name mismatch.
        if crate::adapter::registry::pi_family_managed(provider, endpoint)
            && parsed.get("confine").is_none()
        {
            if let Some(pm_dir) = &pm_dir {
                let overrides =
                    crate::doctor::host::read_host_overrides(pm_dir).map_err(Error::rejected)?;
                if overrides.and_then(|o| o.confine_pi_workers) == Some(true) {
                    let mut obj = parsed.as_object().cloned().unwrap_or_default();
                    obj.insert("confine".to_string(), json!(true));
                    parsed = Value::Object(obj);
                    defaulted = true;
                }
            }
        }
        // Only re-serialize when the default actually changed the set —
        // the caller's raw `params` string is stored verbatim otherwise.
        let defaulted_params;
        let agent_params = if defaulted {
            defaulted_params = serde_json::to_string(&parsed)
                .map_err(|e| Error::internal(format!("params reserialize: {e}")))?;
            Some(defaulted_params.as_str())
        } else {
            agent_params
        };
        // CAD-559: a pi agent registers with its launch model already
        // decided — explicit `--model`, else whatever `model_defaults`
        // resolves (role/provider entries; `provider_default` leaves the
        // slot empty for pi), else `[pi].models.default.worker`, else
        // refuse. Whatever lands must be on `[pi].models.allow`; the
        // adapter re-checks the stored value at every open. The gate
        // sees the EFFECTIVE params — a [host]-defaulted `confine`
        // survives into the stored row.
        let mut pi_params: Option<String> = None;
        let mut pi_selection: Option<Value> = None;
        let parsed = if crate::adapter::registry::pi_family_managed(provider, endpoint) {
            // `provider_default` means "whatever the provider picks" —
            // the exact silent fallback this gate removes, so pi
            // refuses it outright rather than collide downstream.
            if crate::model_defaults::parse_model_policy(model_policy)?
                == crate::model_defaults::ModelPolicy::ProviderDefault
            {
                return Err(Error::rejected(
                    "pi has no provider default — pass --model <provider/id> or \
                     set the Pi role default in Settings > Agents (CAD-559)",
                ));
            }
            let defaults = self.store.model_defaults()?;
            let resolved = crate::model_defaults::resolve(crate::model_defaults::ResolveRequest {
                provider,
                endpoint_kind: endpoint,
                runtime_role: role,
                team_role,
                model_policy,
                params: agent_params,
                config: &defaults.config,
                revision: defaults.revision,
            })?;
            let picked = resolved
                .model_selection
                .as_ref()
                .and_then(|sel| sel.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if resolved.model_selection.is_none() {
                // An endpoint that cannot carry a model (inbox) has no
                // launch model to gate.
                parsed
            } else {
                // No tracker dir means no policy — an absent `[pi]`
                // allows nothing either way.
                let policy = self.stored_pi_policy()?;
                // CAD-575: the allowlist the gate checks is the role's
                // own when pm.yaml carries one (master_allow /
                // worker_allow), else `allow`.
                let role_key = if crate::master::is_master(alias) {
                    "master"
                } else {
                    "worker"
                };
                match picked {
                    // The defaults layer resolved a model — run the
                    // gate on it and pass the caller's params through
                    // so `register_agent` re-derives that provenance
                    // (explicit, role_default, provider_baseline)
                    // instead of naming it explicit.
                    Some(model) => {
                        crate::pi_policy::require_allowed(policy.as_ref(), role_key, &model)?;
                        crate::adapter::pi::refuse_confined_devin(
                            alias,
                            parsed.get("confine").and_then(Value::as_bool) == Some(true)
                                && role_key == "worker",
                            &model,
                        )?;
                        parsed
                    }
                    // Nothing resolved — `[pi].models.default` fills
                    // the slot. The row stores the launch model (the
                    // adapter replays params at open) and the
                    // provenance names the operator's policy, not a
                    // caller flag.
                    None => {
                        let model =
                            crate::pi_policy::resolve_model(policy.as_ref(), role_key, None)?;
                        crate::adapter::pi::refuse_confined_devin(
                            alias,
                            parsed.get("confine").and_then(Value::as_bool) == Some(true)
                                && role_key == "worker",
                            &model,
                        )?;
                        let mut merged = match resolved.params.as_deref() {
                            Some(raw) => serde_json::from_str::<Value>(raw)?
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                            None => serde_json::Map::new(),
                        };
                        merged.insert("model".to_string(), json!(model));
                        let text = Value::Object(merged).to_string();
                        pi_params = Some(text.clone());
                        pi_selection = Some(crate::model_defaults::pi_policy_default_selection(
                            resolved.team_role.as_deref().unwrap_or(role),
                            &model,
                        ));
                        serde_json::from_str::<Value>(&text)?
                    }
                }
            }
        } else {
            parsed
        };
        self.authorize_register(alias, &parsed, peer_pid)?;
        let registered = self.store.register_agent(&crate::store::NewAgent {
            alias,
            provider,
            endpoint_kind: endpoint,
            role,
            cwd: &cwd.to_string_lossy(),
            sandbox,
            instructions,
            params: pi_params.as_deref().or(agent_params),
            team_role,
            model_policy,
        });
        registered?;
        // CAD-559: when `[pi].models.default` filled the launch model,
        // `register_agent` labeled it `explicit` — restamp the real
        // provenance before the row can be read or launched.
        if let Some(selection) = &pi_selection {
            self.store.set_model_selection(alias, selection)?;
        }
        // A mailbox has no actor — it is `idle` with its pseudo-endpoint
        // from registration and simply accrues queued messages.
        if !registry::has_actor(provider, endpoint) {
            return Ok(json!({
                "alias": alias, "state": "idle", "provider": provider,
                "endpoint": format!("inbox://{alias}"),
            }));
        }
        self.launch_actor(alias)?;
        Ok(json!({"alias": alias, "state": "starting", "provider": provider}))
    }

    /// CAD-1752: `cadence agent instructions` — replace an agent's
    /// standing instructions. Operator only, by the connection: an agent
    /// (the target itself and its PM included) is refused, so this is
    /// stricter than `agent set`'s `authorize_agent_mutation`.
    pub(super) fn rpc_instructions_set(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection_on_agent("agent instructions", params, peer_pid)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let text = required_str(params, "instructions")?;
        if crate::master::is_master(&alias) {
            return Err(Error::rejected(
                "the master's instructions are its SOUL and AGENT files, not this verb",
            ));
        }
        let agent = self.store.agent(&alias)?;
        if agent
            .params
            .as_ref()
            .is_some_and(|p| p.get("profile").is_some())
        {
            return Err(Error::rejected(
                "a profile agent's job comes from its settings profile, not from instructions",
            ));
        }
        let audit = json!({"by": "operator", "by_kind": "operator", "caller_pid": peer_pid});
        let (old, new) = self.store.set_instructions_by(&alias, text, &audit)?;
        // `applies` here is what the DAEMON alone does: only codex and Devin
        // cloud read the stored row, and only when a session opens
        // (`next_launch`); every other provider is `not_delivered` by this RPC.
        // The CLI verb and the board route then rewrite the agent's briefing and
        // queue a re-read notice (`briefing::deliver_job_update`) and, on
        // success, report `next_turn` instead - the two-step split is
        // deliberate, so the raw RPC never promises what only a wrapper does.
        // Nothing here restarts the agent.
        let applies = match (agent.provider.as_str(), agent.endpoint_kind.as_str()) {
            ("codex", _) | ("devin", "cloud") => "next_launch",
            _ => "not_delivered",
        };
        Ok(json!({
            "alias": alias, "state": "updated", "applies": applies,
            "old_sha256": old, "new_sha256": new,
        }))
    }

    /// `agent_operational` — the narrow, operator-only operational
    /// projection of ordinary registered agents (CAD-2207). It carries only
    /// the process/lifecycle fields fleet lifecycle needs: runtime `alias`,
    /// uid, pid/start, provider/kind, generation, state, liveness flags, a
    /// bounded `{id,state}` list of in-flight rows and the unknown-turn
    /// count. It never carries a display name, avatar, profile, params or
    /// message body.
    ///
    /// CAD-2207 no-list: an agent whose alias has a stored `assistants`
    /// binding is a member's personal assistant. It is withheld from this
    /// projection for EVERY caller, the operator and an unproven caller
    /// included — operator rights are not the member's ownership proof. The
    /// owner reads through the signed `member_*` route. Ordinary agents keep
    /// the full row. A board that needs pid→agent attribution uses
    /// [`Self::rpc_board_peer_agent`], which classifies a concrete live TCP
    /// peer against ALL roots and returns only an opaque agent marker for a
    /// bound assistant.
    pub(super) fn rpc_agent_operational(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("agent operational", params, peer_pid)?;
        let mut agents: Vec<Value> = Vec::new();
        for agent in self.store.agents()? {
            if self.store.assistant_for_alias(&agent.alias)?.is_some() {
                continue;
            }
            let (dead, resumable) = self.agent_liveness(&agent);
            let unknown = self.store.unknown_messages(&agent.alias)?.len();
            let in_flight: Vec<Value> = self
                .store
                .messages(&agent.alias)?
                .into_iter()
                .filter(|m| matches!(m.state.as_str(), "running" | "submitted" | "unknown"))
                .take(32)
                .map(|m| json!({"id": m.id, "state": m.state}))
                .collect();
            agents.push(json!({
                "alias": agent.alias,
                "uid": agent.uid,
                "provider": agent.provider,
                "endpoint_kind": agent.endpoint_kind,
                "state": agent.state,
                "pid": agent.pid,
                "pid_start": agent.pid_start,
                "generation": agent.generation,
                "thread_id": agent.thread_id,
                "session_id": agent.session_id,
                "needs_native_resume": agent.endpoint.is_none()
                    && (agent.thread_id.is_some() || agent.session_id.is_some()),
                "endpoint": agent.endpoint,
                "dead": dead,
                "resumable": resumable,
                "busy": !in_flight.is_empty(),
                "unknown": unknown,
                "messages": in_flight,
            }));
        }
        Ok(json!({"agents": agents}))
    }

    /// CAD-2207: the one all-agent classifier a board may use for a concrete
    /// live TCP four-tuple. It resolves the socket's owning processes the way
    /// `ss -p` does, walks their ancestry/ptys against EVERY agent root
    /// (pty panes and managed provider roots), and answers:
    ///
    /// - `{"agent": {"kind": "named", "alias": "..."}}` — an ordinary agent,
    ///   an alias this caller already sees in `agent_operational`;
    /// - `{"agent": {"kind": "opaque"}}` — an agent whose alias is withheld
    ///   (a bound member assistant): the board must still treat it as an
    ///   agent and revoke a human session, but learns no identity;
    /// - `{"agent": null}` — no agent on the peer.
    ///
    /// There is no bound-only verb and no pid list: the caller names a live
    /// connection, the daemon resolves it. Ambiguity or an unreadable ancestry
    /// is a fixed generic refusal (never the aliases `peer::tcp_peer_agent`
    /// keeps internally), so a board fails closed.
    pub(super) fn rpc_board_peer_agent(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.board_operator("board peer agent", params, peer_pid)?;
        let server = peer_addr_param(params, "server")?;
        let peer = peer_addr_param(params, "peer")?;
        self.verify_board_peer(peer_pid, server, peer)?;
        let roots = self.all_agent_roots()?;
        let matched = crate::peer::tcp_peer_agent(server, peer, &roots)
            .map_err(|_| Error::rejected("peer identity is ambiguous or unprovable"))?;
        match matched {
            None => Ok(json!({"agent": Value::Null})),
            Some(alias) => {
                if self.store.assistant_for_alias(&alias)?.is_some() {
                    Ok(json!({"agent": {"kind": "opaque"}}))
                } else {
                    Ok(json!({"agent": {"kind": "named", "alias": alias}}))
                }
            }
        }
    }

    /// CAD-2207: positive operator proof for a concrete board TCP peer, run
    /// against ALL roots so a bound assistant's process can never prove
    /// operator authority. The result is only `{"operator": true}` or a
    /// fixed generic refusal; no alias, pane or enrollment text crosses the
    /// wire.
    pub(super) fn rpc_board_peer_operator(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.board_operator("board peer operator", params, peer_pid)?;
        let server = peer_addr_param(params, "server")?;
        let peer = peer_addr_param(params, "peer")?;
        self.verify_board_peer(peer_pid, server, peer)?;
        let roots = self.all_agent_roots()?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        crate::peer::tcp_peer_operator_proof(
            server,
            peer,
            uid,
            std::process::id(),
            &roots,
            &self.state_dir,
        )
        .map_err(|_| Error::rejected("peer is not provably the operator"))?;
        Ok(json!({"operator": true}))
    }

    /// CAD-2207: the aggregate restart gate, computed over EVERY agent row,
    /// ordinary and restricted alike. `ignored_stale` is the ordinary
    /// `(alias, message_id)` pairs a caller is already carrying under
    /// `--ignore-stale`; they are excluded, so ordinary ignore semantics
    /// survive. `busy`/`stale` mirror `busy_agents`: in-flight
    /// `running`/`submitted`, a pty probe, and the dead/stopped/offline/
    /// attention holder classification. `starting`/`stopping` are live
    /// blockers, not stale. The answer has no per-agent data: a caller learns
    /// only the two aggregate booleans.
    pub(super) fn rpc_agent_fleet_state(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.board_operator("agent fleet state", params, peer_pid)?;
        let ignored = ignored_stale_pairs(params)?;
        let (mut busy, mut stale) = (false, false);
        for agent in self.store.agents()? {
            let state = agent.state.as_str();
            let restricted = self.store.assistant_for_alias(&agent.alias)?.is_some();
            let (dead, _) = self.agent_liveness(&agent);
            let has_inflight = self
                .store
                .messages(&agent.alias)?
                .into_iter()
                .filter(|m| matches!(m.state.as_str(), "running" | "submitted"))
                // A caller-supplied ignored pair never exempts a private turn.
                .any(|m| {
                    restricted
                        || !ignored
                            .iter()
                            .any(|(alias, ignored_id)| alias == &agent.alias && ignored_id == &m.id)
                });
            if has_inflight {
                if matches!(state, "starting" | "stopping") {
                    busy = true;
                } else if matches!(state, "stopped" | "offline" | "attention") || dead {
                    stale = true;
                } else {
                    busy = true;
                }
            }
            if agent.endpoint_kind == "pty" && agent.endpoint.is_some() {
                if let Ok(probe) = self
                    .adapter_for(&agent.alias)
                    .and_then(|adapter| adapter.probe())
                {
                    if !probe.idle {
                        busy = true;
                    }
                }
            }
        }
        let settle = self.settle_state()?;
        let drain = self.drain_state();
        Ok(json!({
            "busy": busy,
            "stale": stale,
            "settle": settle,
            "drain": drain,
            "complete": self.resume_complete()?,
            "instance": self.instance.as_str(),
        }))
    }

    /// CAD-2207: one undifferentiated full-fleet resume verdict. `false`
    /// means at least one actor cannot be resumed under operator authority
    /// (for example, a member-authorized runtime); the caller gets no
    /// alias, count, or class field — only the aggregate incompleteness.
    fn resume_complete(&self) -> Result<bool> {
        for agent in self.store.agents()? {
            if self.store.assistant_for_alias(&agent.alias)?.is_some()
                && registry::has_actor(&agent.provider, &agent.endpoint_kind)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// CAD-2207: did every agent captured live before the restart come back
    /// live under the SAME immutable uid? This never serializes an alias,
    /// uid or count: the caller gets one enum. It covers ordinary and bound
    /// actors alike, so a cold-start verdict is not a private-only oracle.
    ///
    /// A marker record is authoritative only when it was STORED by the
    /// predecessor; missing/old evidence falls back to an all-fleet
    /// actor-risk check. Actual ownership, thread, adapter endpoint and
    /// `agent_liveness` decide "live" — a stale `idle`/`busy` label alone
    /// never settles.
    fn settle_state(&self) -> Result<&'static str> {
        const SETTLE_TIMEOUT_SECS: f64 = 120.0;
        let evidence = self.settle_evidence.lock().unwrap().clone();
        let Some(evidence) = evidence else {
            return Ok(if self.fleet_runtime_risk()? {
                "unverified"
            } else {
                "settled"
            });
        };
        let rows = match evidence {
            Err(_) => return Ok("unverified"),
            Ok(rows) => rows,
        };
        if rows.is_empty() {
            return Ok("settled");
        }
        let deadline = self.started_at + SETTLE_TIMEOUT_SECS;
        let mut pending = false;
        for (alias, expected_uid) in rows {
            let agent = match self.store.agent(&alias) {
                Ok(agent) => agent,
                Err(_) => return Ok("failed"),
            };
            if agent.uid != expected_uid {
                return Ok("failed");
            }
            let state = agent.state.as_str();
            if matches!(state, "attention" | "stopped" | "offline") {
                return Ok("failed");
            }
            let ctl = self.lifecycle.lock().unwrap().agents.get(&alias).cloned();
            let Some(ctl) = ctl else {
                return Ok("failed");
            };
            if ctl_finished(&ctl) {
                return Ok("failed");
            }
            if matches!(state, "starting" | "stopping") {
                pending = true;
                continue;
            }
            if !matches!(state, "idle" | "busy" | "running" | "waiting_input") {
                return Ok("failed");
            }
            if !agent.enabled {
                return Ok("failed");
            }
            let (dead, _) = self.agent_liveness(&agent);
            if dead {
                return Ok("failed");
            }
            if ctl.adapter.lock().unwrap().is_none() {
                return Ok("failed");
            }
            if registry::attachable(&agent.provider, &agent.endpoint_kind)
                && agent.endpoint.is_none()
            {
                return Ok("failed");
            }
            if agent.endpoint_kind == "pty"
                && !crate::adapter::pty::pane_alive(&self.state_dir, &alias, &self.provider_env)
            {
                return Ok("failed");
            }
        }
        if pending {
            return Ok(if crate::rollout::unix_now() >= deadline {
                "unverified"
            } else {
                "pending"
            });
        }
        Ok("settled")
    }

    /// CAD-2207: a cold start with missing/old/malformed evidence cannot
    /// prove that no actor work existed. Any actor-capable row — ordinary
    /// or bound — makes the verdict unverified. There is deliberately no
    /// personal-binding branch here, so the public enum is not a
    /// private-activity oracle.
    fn fleet_runtime_risk(&self) -> Result<bool> {
        for agent in self.store.agents()? {
            if registry::has_actor(&agent.provider, &agent.endpoint_kind) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// CAD-2207: predecessor drain evidence without a per-alias channel.
    /// Strictly validates the successor's `last-recovery.json`; missing or
    /// malformed fields are `unverified`, a recorded drain failure or any
    /// fenced/unevidenced turn is `failed`, and only a bound clean record
    /// is `clean`. Aliases in fenced rows are read, never returned.
    fn drain_state(&self) -> &'static str {
        let raw =
            match std::fs::read_to_string(self.state_dir.join(crate::daemon::LAST_RECOVERY_FILE)) {
                Ok(raw) => raw,
                Err(_) => return "unverified",
            };
        let value: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => return "unverified",
        };
        let Some(obj) = value.as_object() else {
            return "unverified";
        };
        if obj.get("instance").and_then(Value::as_str) != Some(self.instance.as_str()) {
            return "unverified";
        }
        let failed = match obj.get("consumed") {
            Some(Value::Null) => false,
            Some(Value::Object(consumed)) => {
                if !consumed.get("instance").is_some_and(|v| v.is_string())
                    || !consumed
                        .get("stale")
                        .is_some_and(|v| v.is_null() || v.is_string())
                {
                    return "unverified";
                }
                match consumed.get("failed") {
                    Some(Value::Null) => false,
                    Some(Value::String(_)) => true,
                    _ => return "unverified",
                }
            }
            _ => return "unverified",
        };
        let Some(fenced) = recovery_rows(obj.get("fenced")) else {
            return "unverified";
        };
        let Some(unevidenced) = recovery_rows(obj.get("unevidenced")) else {
            return "unverified";
        };
        if failed || !fenced.is_empty() || !unevidenced.is_empty() {
            "failed"
        } else {
            "clean"
        }
    }

    /// Every registered agent row as a dead-drop attribution root, ordinary
    /// and bound alike. Used only inside the daemon by the board peer verbs;
    /// nothing serializes it.
    fn all_agent_roots(&self) -> Result<crate::peer::AgentRoots> {
        let mut panes = Vec::new();
        let mut managed = Vec::new();
        for agent in self.store.agents()? {
            let Some(pid) = agent.pid.and_then(|pid| u32::try_from(pid).ok()) else {
                continue;
            };
            let row = (
                agent.alias.clone(),
                pid,
                agent.pid_start.and_then(|start| u64::try_from(start).ok()),
            );
            if agent.endpoint_kind == "pty" && agent.generation.is_some() {
                panes.push(row);
            } else if pid > 1
                && registry::enrolls_build_slots(&agent.provider, &agent.endpoint_kind)
            {
                managed.push(row);
            }
        }
        Ok(crate::peer::AgentRoots {
            panes: crate::peer::AgentPids::classify(panes),
            managed: crate::peer::AgentPids::classify(managed),
        })
    }

    /// CAD-2207: the supplied four-tuple must be the connection THIS board
    /// process is serving, not a caller-chosen live socket. The Unix peer must
    /// hold both the listening socket at `server` and the accepted server-side
    /// socket. Where the state dir has persisted `ui.json`, the server address
    /// must also match that configured listener; otherwise the server-side
    /// ownership is the proof. Unprovable or mismatched tuples refuse with
    /// generic text. Same-UID fd duplication stays a CAD-2148 residual.
    fn verify_board_peer(
        &self,
        peer_pid: u32,
        server: std::net::SocketAddr,
        peer: std::net::SocketAddr,
    ) -> Result<()> {
        crate::peer::server_socket_owned_by(server, peer, peer_pid).map_err(|_| {
            Error::rejected("the board peer does not own the live connection's server side")
        })?;
        if crate::ui::opts_present(&self.state_dir) {
            let opts = crate::ui::persisted_opts(&self.state_dir);
            let host = opts.host.as_deref().unwrap_or("127.0.0.1");
            let port = opts.port.unwrap_or(3010);
            let ip: std::net::IpAddr = host
                .parse()
                .map_err(|_| Error::rejected("the board's configured listener cannot be proved"))?;
            let configured = std::net::SocketAddr::new(ip, port);
            if crate::peer::canonical(configured) != crate::peer::canonical(server) {
                return Err(Error::rejected(
                    "the board peer does not match the configured listener",
                ));
            }
        }
        Ok(())
    }

    /// [`Self::operator_connection`] for the board peer verbs: a refusal must
    /// never echo an agent alias from the caller's ancestry. Identity-field
    /// refusals keep their specific text; every other reason is generic.
    fn board_operator(&self, verb: &str, params: &Value, peer_pid: u32) -> Result<()> {
        self.operator_connection(verb, params, peer_pid)
            .map_err(|error| {
                if error.to_string().contains("authority is connection-bound") {
                    error
                } else {
                    Error::rejected(format!(
                        "{verb} refused: the caller is not provably the operator"
                    ))
                }
            })
    }

    /// CAD-1989: `POST /api/agent-profiles` - a catalogue hire saves a settings
    /// profile and launches nothing. Operator only, by the connection.
    pub(super) fn rpc_agent_profile_create(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection_on_agent("agent profile create", params, peer_pid)
            .map_err(|e| {
                Error::rejected(format!(
                    "agent profile create refused: a hire is the operator's act ({e})"
                ))
            })?;
        let alias = required_str(params, "alias")?;
        let id = required_str(params, "catalogue")?;
        let role = crate::agent_roles::find(id).ok_or_else(|| {
            Error::rejected(format!(
                "Unknown catalogue role '{id}' - one of: {}",
                crate::agent_roles::ids()
            ))
        })?;
        crate::agent_settings::create_catalogue_profile(
            &self.store,
            alias,
            role,
            role.look,
            optional_text(params, "job")?,
            optional_text(params, "model")?,
        )
    }

    /// CAD-1989: the hosted `agent_profile_context` envelope for a settings
    /// profile. The job is the operator's: the operator, or an agent whose
    /// own runtime row is registered against this profile, may read it;
    /// every other caller is refused.
    pub(super) fn rpc_agent_profile_context(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let id = required_str(params, "id")?;
        if self.operator_evidence(peer_pid).is_err() {
            let own = match self.caller_identity(peer_pid)? {
                Caller::Agent(verified) => {
                    verified
                        .agent
                        .params
                        .as_ref()
                        .and_then(|p| p.get("profile"))
                        .and_then(Value::as_str)
                        == Some(id)
                }
                Caller::NoAgentIdentity => false,
            };
            if !own {
                return Err(Error::rejected(
                    "agent profile context is the operator's, or the context of a runtime \
                     registered against that profile",
                ));
            }
        }
        crate::agent_settings::profile_context(&self.store, &self.store.agents()?, id)
    }

    /// CAD-1752: the role catalogue and what this install registers on. A
    /// read: the catalogue is static and carries no model (CAD-1989).
    pub(super) fn rpc_agent_roles(self: &Arc<Self>) -> Result<Value> {
        let provider = "pi";
        let endpoint = registry::default_kind(provider)?;
        let roles: Vec<Value> = crate::agent_roles::ROLES
            .iter()
            .map(|role| role.to_json())
            .collect();
        Ok(json!({
            "categories": crate::agent_roles::CATEGORIES
                .iter()
                .map(|(id, label)| json!({"id": id, "label": label}))
                .collect::<Vec<_>>(),
            "roles": roles,
            "install": {
                "hosted": crate::issue::cloud::hosted_company(|key| std::env::var(key).ok()).is_ok(),
                "provider": provider,
                "endpoint_kind": endpoint,
            },
        }))
    }

    pub(super) fn rpc_events(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let raw = required_str(params, "alias")?;
        // The daemon stream has no agents row — read-only, so a plain
        // match suffices; every other RPC keeps resolving to an agent.
        let daemon = raw == DAEMON_ALIAS;
        let alias = if daemon {
            raw.to_string()
        } else {
            self.resolve_alias(raw)?
        };
        // CAD-2207: a bound member assistant may read only events above the
        // immutable cutoff frozen when its binding was created (or events
        // minted after it). Legacy same-alias history — including a prior
        // ordinary agent's — stays withheld. An unprovable binding/fence
        // refuses rather than serving it; ordinary agents keep the old read.
        let floor = if daemon {
            0
        } else {
            match self.store.assistant_for_alias(&alias)? {
                None => 0,
                Some(binding) => {
                    let Some(fence) = self
                        .store
                        .assistant_event_fence(&binding.org, &binding.sub)?
                    else {
                        return Err(Error::rejected(format!(
                            "agent events refused: the member assistant binding for \
                             '{alias}' has no event cutoff (CAD-2207)"
                        )));
                    };
                    if fence.alias != binding.alias || fence.assistant_id != binding.assistant_id {
                        return Err(Error::rejected(format!(
                            "agent events refused: the member assistant binding for \
                             '{alias}' does not match its recorded event cutoff (CAD-2207)"
                        )));
                    }
                    let Some(agent) = self.store.agent_opt(&alias)? else {
                        return Err(Error::rejected(format!(
                            "agent events refused: the member assistant agent for \
                             '{alias}' is not registered (CAD-2207)"
                        )));
                    };
                    if let Some(uid) = &fence.agent_uid {
                        if uid != &agent.uid {
                            return Err(Error::rejected(format!(
                                "agent events refused: the member assistant agent for \
                                 '{alias}' no longer matches its recorded event cutoff \
                                 (CAD-2207)"
                            )));
                        }
                    } else {
                        self.store.assistant_event_fence_bind_agent(
                            &binding.org,
                            &binding.sub,
                            &agent.uid,
                        )?;
                    }
                    fence.cutoff_seq
                }
            }
        };
        // CAD-1853 review: the alias' event stream carries cloud request and
        // result detail, so it applies the same source/owner decision as the
        // dedicated cloud read before projecting anything — current READ
        // authority, and only the caller's own source's rows.
        let mut projection = self.admit_cloud_projection(&alias, peer_pid, "agent events")?;
        // CAD-1380: daemon `assist_intent` rows are member-private routing. Only
        // the operator reads them from the shared stream; every other caller gets
        // the same page with those rows removed, cursors unchanged.
        let hide_intents = daemon
            && !matches!(
                self.connection_caller(peer_pid)?,
                caller_rule::Who::Operator
            );
        let visible = |projection: &cloud_agent_rpc::CloudProjection,
                       events: Vec<crate::store::Event>|
         -> Result<Vec<crate::store::Event>> {
            let events = projection.events(&self.store, events)?;
            Ok(if hide_intents {
                events
                    .into_iter()
                    .filter(|e| e.kind != "assist_intent")
                    .collect()
            } else {
                events
            })
        };
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Event cursor must be nonnegative"));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        // `tail` is the newest-first default `cadence events` uses
        // when no --after cursor is given: one bounded page ending at
        // the latest seq, oldest first within it, plus the forward
        // cursor and whether older history exists below the page.
        if params.get("tail").and_then(Value::as_bool).unwrap_or(false) {
            let mut events = self.store.events_tail_after(&alias, floor, 51)?;
            let has_older = events.len() > 50;
            // The store returns oldest first; discard the extra oldest row.
            if has_older {
                events.remove(0);
            }
            // Observed before it is projected: an event naming a row the
            // admission snapshot does not cover — a routed cloud result that
            // resolved since, or a turn that settled meanwhile — re-resolves
            // this page's admission once, exactly as the waiting path does.
            if projection.needs_readmission(
                &self.store,
                events
                    .iter()
                    .filter_map(|event| event.payload.get("message").and_then(Value::as_str)),
            )? {
                projection = self.admit_cloud_projection(&alias, peer_pid, "agent events")?;
            }
            let cursor = events.last().map(|e| e.seq).unwrap_or(0);
            let mut page = json!({
                "events": visible(&projection, events)?.iter().map(crate::store::Event::to_json).collect::<Vec<_>>(),
                "cursor": cursor,
                "has_older": has_older,
            });
            if let Some(reason) = projection.withheld {
                page["cloud_withheld"] = json!(reason);
            }
            return Ok(page);
        }
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let events = self.store.events_after(&alias, floor, after, 100)?;
            if !events.is_empty() || self.closing.load(Ordering::SeqCst) {
                // A waiting read can observe an event about a turn that
                // settled after this admission was taken — or, on an ordinary
                // alias, an event naming a routed cloud result that resolved
                // since. Re-resolve the page's whole admission before
                // projecting it, exactly as `thread_read` does: the old
                // admission never covered those rows. This page is returned
                // either way, so one re-resolution is the bound.
                if projection.needs_readmission(
                    &self.store,
                    events
                        .iter()
                        .filter_map(|event| event.payload.get("message").and_then(Value::as_str)),
                )? {
                    projection = self.admit_cloud_projection(&alias, peer_pid, "agent events")?;
                }
                let cursor = events.last().map(|e| e.seq).unwrap_or(after);
                let mut page = json!({
                    "events": visible(&projection, events)?.iter().map(crate::store::Event::to_json).collect::<Vec<_>>(),
                    "cursor": cursor,
                });
                if let Some(reason) = projection.withheld {
                    page["cloud_withheld"] = json!(reason);
                }
                return Ok(page);
            }
            if Instant::now() >= deadline {
                let mut page = json!({"events": [], "cursor": after});
                if let Some(reason) = projection.withheld {
                    page["cloud_withheld"] = json!(reason);
                }
                return Ok(page);
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// Operator readiness claim for gated endpoints (pty): asserts the
    /// terminal was inspected and is idle with an empty input. Single
    /// use, short TTL — see the adapter for semantics. `by` is the
    /// claimer the caller rule attributed (CAD-384): the agent whose pane
    /// the call came from, else the proven operator — recorded for audit
    /// (G5 policy stays open; the record exists).
    pub(super) fn rpc_ready(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let by = required_str(params, "by")?.to_string();
        let force = params
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // The claim itself runs the idle probe and refuses a busy
        // pane — `force` is the operator's explicit override and is
        // recorded as such on the event.
        let probe = self
            .adapter_for(&alias)?
            .claim_ready(Some(by.clone()), force)?;
        let mut detail = json!({
            "by": by,
            "probe": probe.to_json(),
        });
        if force {
            detail["forced"] = json!(true);
        }
        let _ = self.store.event_public(&alias, "ready_claimed", detail);
        // Wake the actor's gate wait — a claim should release the head
        // message immediately, not on the next poll tick.
        self.notify_agent(&alias);
        self.wake();
        Ok(json!({"alias": alias, "state": "ready-claimed"}))
    }

    /// Screen contents of a PTY endpoint for operator inspection.
    pub(super) fn rpc_capture(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        if self.store.app_material_endpoint(&alias)? {
            self.operator_connection("app endpoint transcript inspection", params, peer_pid)?;
        }

        let text = self.adapter_for(&alias)?.capture()?;
        Ok(json!({"alias": alias, "capture": text}))
    }

    /// Screen probe for a PTY endpoint — the same reduction the
    /// verified auto-claim gate uses.
    pub(super) fn rpc_probe(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        if self.store.app_material_endpoint(&alias)? {
            self.operator_connection("app endpoint transcript inspection", params, peer_pid)?;
        }

        let probe = self.adapter_for(&alias)?.probe()?;
        let mut out = probe.to_json();
        out["alias"] = json!(alias);
        Ok(out)
    }

    /// Merge key=value pairs into an agent's stored params — how an
    /// existing agent opts into `auto_ready=verified` post-launch.
    ///
    /// Caller rule (CAD-149, [`Self::authorize_agent_mutation`]): the
    /// operator and the target's own PM may set any allowed key; the
    /// agent itself only `--next-launch` model/effort
    /// ([`registry::ParamClass::SelfService`]); anyone else nothing.
    /// Every accepted change records `params_updated` with the derived
    /// caller and each key's old and new value.
    pub(super) fn rpc_set(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let mut patch = params
            .get("patch")
            .filter(|p| p.is_object())
            .cloned()
            .ok_or_else(|| Error::rejected("Missing 'patch' object"))?;
        // Live-mutable params are an explicit allowlist — arbitrary keys
        // like `upstream` or `session` would silently rewire routing and
        // session binding, so they are rejected rather than merged.
        let agent = self.store.agent(&alias)?;
        if agent.archived_at.is_some() {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is archived — restore it before changing it"
            )));
        }
        // `next_launch`: launch params (model, effort) stored for the
        // next open only — the live process is left exactly as it is.
        let next_launch = params
            .get("next_launch")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // Who may change these keys — decided before any validation, so
        // a refused caller learns nothing about the target's params.
        let self_service = next_launch
            && patch
                .as_object()
                .unwrap()
                .keys()
                .all(|k| registry::param_class(k) == registry::ParamClass::SelfService);
        let mutation = if self_service {
            AgentMutation::SelfService
        } else {
            AgentMutation::Controlled
        };
        let caller =
            self.authorize_agent_mutation(params, peer_pid, "agent set", &agent, mutation)?;
        // CAD-804: `gc_keep` is a registry keep, not an endpoint param.
        // Take it out before the endpoint-param allowlist runs; every
        // other key still validates and merges exactly as before.
        let gc_keep = match patch.as_object_mut().unwrap().remove("gc_keep") {
            None => None,
            Some(Value::Bool(keep)) => Some(keep),
            Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Some(true),
            Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Some(false),
            Some(Value::Null) => Some(false),
            Some(_) => {
                return Err(Error::invalid(
                    "invalid_gc_keep",
                    "gc_keep must be true or false",
                ))
            }
        };
        if next_launch && gc_keep.is_some() {
            return Err(Error::rejected(
                "gc_keep is not a launch param; set it without --next-launch",
            ));
        }
        for (key, value) in patch.as_object().unwrap() {
            if key == "launch_workdir" {
                return Err(Error::rejected(
                    "params 'launch_workdir' is set by the daemon, not the caller",
                ));
            }
            proto::param_key(key)?;
            if matches!(
                key.as_str(),
                "profile" | "profile_revision" | "profile_digest"
            ) {
                return Err(Error::rejected(
                    "saved profile provenance is immutable; launch a new session",
                ));
            }
            if next_launch {
                registry::validate_next_launch_param(
                    &agent.provider,
                    &agent.endpoint_kind,
                    key,
                    value,
                )?;
            } else {
                registry::validate_live_param(&agent.provider, &agent.endpoint_kind, key, value)?;
            }
        }
        // CAD-559: a pi agent's model is on the operator's allowlist or
        // it is refused — and it can never be cleared, because a pi
        // launch without `--model` would silently fall back. CAD-575:
        // the role's own list applies when pm.yaml carries one.
        if crate::adapter::registry::pi_family_provider(&agent.provider)
            && patch.as_object().unwrap().contains_key("model")
        {
            let policy = self.stored_pi_policy()?;
            let role = if crate::master::is_master(&agent.alias) {
                "master"
            } else {
                "worker"
            };
            match patch["model"].as_str() {
                Some(model) => crate::pi_policy::require_allowed(policy.as_ref(), role, model)?,
                None => {
                    return Err(Error::rejected(
                        "agent set refused: a pi agent always launches on an \
                         explicit allowlisted model — the model key cannot be \
                         cleared (CAD-559)",
                    ))
                }
            }
        }
        if crate::adapter::registry::pi_family_provider(&agent.provider) {
            let mut effective = agent.params.clone().unwrap_or_else(|| json!({}));
            if let (Some(current), Some(changes)) = (effective.as_object_mut(), patch.as_object()) {
                current.extend(changes.clone());
            }
            crate::adapter::pi::refuse_confined_devin(
                &alias,
                effective.get("confine").and_then(Value::as_bool) == Some(true)
                    && !crate::master::is_master(&alias),
                effective
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )?;
        }
        let mut audit = caller_audit(&caller);
        audit["caller_pid"] = json!(peer_pid);
        audit["next_launch"] = json!(next_launch);
        if let Some(keep) = gc_keep {
            self.store.set_gc_keep_by(&alias, keep, &audit)?;
        }
        if patch.as_object().unwrap().is_empty() {
            if gc_keep.is_none() {
                return Err(Error::rejected("agent set needs at least one key"));
            }
            self.wake();
            return Ok(json!({"alias": alias, "state": "updated", "gc_keep": gc_keep}));
        }
        self.store.set_params_by(&alias, &patch, &audit)?;
        if next_launch {
            self.wake();
            return Ok(json!({"alias": alias, "state": "updated", "applies": "next launch"}));
        }
        // Push the merged params into the live adapter so cached
        // endpoint options (auto_ready) take effect without a restart.
        if let Ok(adapter) = self.adapter_for(&alias) {
            if let Some(params) = self.store.agent(&alias)?.params {
                adapter.update_params(&params);
            }
        }
        // Wake a gate wait — new params may be exactly what it needs.
        self.notify_agent(&alias);
        self.wake();
        Ok(json!({"alias": alias, "state": "updated"}))
    }

    /// Drain an inbox agent's durable queue — messages complete
    /// `via=inbox_read` as they are returned. `wait` long-polls on the
    /// daemon's change signal, the same mechanism `events` uses.
    /// `peek` (CAD-480) returns the same queued set without consuming:
    /// nothing completes, so a reader that crashes or truncates loses
    /// nothing. A peeking reader's lower bound defaults to its
    /// server-side ack cursor (`reader`, default `"default"`) — a
    /// restart resumes after its last `agent_inbox_ack`, not its last
    /// read. `unread` is the live queued count either way.
    ///
    /// The peek is a read, so it keeps the mailbox's unguarded shape
    /// (CAD-251). The drain is a consume: like `agent_inbox_ack`, a
    /// proven **agent** caller may drain only its own inbox — a worker
    /// can never consume another agent's queue — while unattributed and
    /// operator callers pass as they always have. A member assistant's
    /// alias is the exception at dispatch: only that assistant itself may
    /// list its mailbox (CAD-2207), so an operator is refused before this
    /// handler runs.
    pub(super) fn rpc_inbox(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_identity_fields(params, "agent_inbox")?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let peek = params.get("peek").and_then(Value::as_bool) == Some(true);
        let reader = inbox_reader(params)?;
        if !peek {
            if let caller_rule::Who::Agent(caller) = self.connection_caller(peer_pid)? {
                if caller != alias {
                    return Err(Error::rejected(format!(
                        "agent_inbox refused: agent '{caller}' cannot drain another \
                         agent's inbox — '{alias}' is consumed by the operator or an \
                         unattributed mailbox reader (caller rule, CAD-480)"
                    )));
                }
            }
        }
        let after = optional_i64(params, "after");
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            // A peek with no explicit `after` resumes at the reader's
            // ack watermark; a drain keeps the legacy bound of 0.
            let bound = match after {
                Some(a) => a,
                None if peek => self.store.inbox_reader_cursor(&alias, reader)?,
                None => 0,
            };
            let messages = if peek {
                self.store.inbox_peek(&alias, bound, reader)?
            } else {
                self.store.inbox_drain(&alias, bound)?
            };
            if !messages.is_empty() || self.closing.load(Ordering::SeqCst) {
                self.wake();
                let cursor = messages.last().map(|m| m.seq).unwrap_or(bound);
                return Ok(json!({
                    "messages": messages.iter().map(Message::to_json).collect::<Vec<_>>(),
                    "cursor": cursor,
                    "unread": self.store.queued_count(&alias)?,
                }));
            }
            if Instant::now() >= deadline {
                return Ok(json!({"messages": [], "cursor": bound,
                                 "unread": self.store.queued_count(&alias)?}));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// Acknowledge an inbox's queued messages through a seq watermark
    /// (CAD-480) — the explicit consume `agent_inbox --peek` leaves to
    /// the reader — plus the reader-cursor housekeeping verbs on the
    /// same channel: `park` marks a queued message so the reader's
    /// peeks skip it (`inbox_park`), `fail` records one failed `--exec`
    /// attempt (`inbox_exec_fail`), and `reset` drops the reader's
    /// cursor (`inbox_ack_reset`, operator only). A mailbox's consumer
    /// has no verifiable identity (CAD-251), so unattributed and
    /// operator callers pass exactly as they do on the read; the one
    /// guard is that an agent may act only on its own alias — a worker
    /// can never consume another agent's inbox.
    ///
    /// The ack is a watermark: `through` (or the greatest `seqs` entry)
    /// completes every still-queued message at or below it, idempotent
    /// on the `queued` state guard, and records the reader's durable
    /// cursor on the `inbox_ack` event. The store clamps `through` to
    /// the inbox's tail before recording it — a claim past the tail
    /// completes what exists but cannot blind the reader to later
    /// arrivals.
    pub(super) fn rpc_inbox_ack(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_identity_fields(params, "agent_inbox_ack")?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let who = self.connection_caller(peer_pid)?;
        let by = match &who {
            caller_rule::Who::Agent(caller) if caller != &alias => {
                return Err(Error::rejected(format!(
                    "agent_inbox_ack refused: agent '{caller}' cannot ack another \
                     agent's inbox — '{alias}' is consumed by the operator or an \
                     unattributed mailbox reader (caller rule, CAD-480)"
                )));
            }
            caller_rule::Who::Agent(caller) => caller.clone(),
            caller_rule::Who::Operator => "operator".to_string(),
            caller_rule::Who::Unproven(_) => "inbox-reader".to_string(),
        };
        let reader = inbox_reader(params)?;
        // `reset` is the destructive action — it re-delivers everything
        // still queued — so it needs operator proof, not merely a
        // consumer's unattributed pass.
        if params.get("reset").and_then(Value::as_bool) == Some(true) {
            if !matches!(who, caller_rule::Who::Operator) {
                return Err(Error::rejected(
                    "agent_inbox_ack --reset refused: resetting a reader cursor is \
                     an operator action — run it from an operator shell outside \
                     every pane (caller rule, CAD-480)",
                ));
            }
            let result = self.store.inbox_ack_reset(&alias, reader, &by)?;
            self.wake();
            return Ok(result);
        }
        if let Some(message) = optional_str(params, "park") {
            let reason = optional_str(params, "reason").unwrap_or("parked");
            let result = self
                .store
                .inbox_park(&alias, reader, message, &by, reason)?;
            self.wake();
            return Ok(result);
        }
        if let Some(fail) = params.get("fail") {
            // One failed `--exec` attempt, surfaced to the event log:
            // the follower reports it, then retries or parks.
            let payload = json!({"reader": reader, "by": by, "fail": fail});
            self.store
                .event_public(&alias, "inbox_exec_fail", payload)?;
            self.wake();
            return Ok(json!({"recorded": "inbox_exec_fail"}));
        }
        let seqs = params
            .get("seqs")
            .and_then(Value::as_array)
            .map(|s| s.iter().filter_map(Value::as_i64).collect::<Vec<_>>())
            .unwrap_or_default();
        let through = optional_i64(params, "through")
            .into_iter()
            .chain(seqs.iter().copied())
            .max()
            .ok_or_else(|| {
                Error::rejected(
                    "agent_inbox_ack needs a seq: pass `seqs` or `through` — \
                     the ack watermark is the greatest",
                )
            })?;
        let result = self.store.inbox_ack(&alias, through, reader, &by)?;
        self.wake();
        Ok(result)
    }

    /// The resume path shared by `agent_resume` and `agent_unfence`:
    /// inbox guard, ownership/fence checks, then start the actor.
    /// Returns whether the actor started — a failed start records
    /// `attention` instead.
    pub(super) fn try_resume(self: &Arc<Self>, alias: &str) -> Result<bool> {
        let agent = self.store.agent(alias)?;
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is an inbox — nothing to resume; \
                 `cadence inbox {alias}` drains it"
            )));
        }
        let mut lc = self.lifecycle.lock().unwrap();
        if lc.owned(alias) {
            // Distinguish the two owned cases for the operator: a live
            // actor means "attach" (fake/managed actors may carry no
            // endpoint address, so state is the signal), a
            // starting/stopping one means "retry".
            let live = agent.endpoint.is_some()
                || matches!(agent.state.as_str(), "idle" | "running" | "waiting_input");
            return if live {
                Err(Error::rejected(format!(
                    "Agent '{alias}' is already live — attach with \
                     `cadence attach {alias}`"
                )))
            } else {
                Err(Error::rejected(format!(
                    "Agent '{alias}' is still starting or stopping — \
                     retry shortly"
                )))
            };
        }
        // An unreconciled `unknown` fences the agent — the exit is an
        // explicit operator reconcile, not another resume (which would
        // fail closed anyway inside start_actor).
        if self.store.has_unknown(alias)? {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is fenced by an unreconciled unknown \
                 message — resume refused. {} {}",
                unknown_inspect_lead(),
                unknown_recovery_note()
            )));
        }
        // Enable only after the ownership/fence checks pass — a
        // rejected resume must leave no side effects behind.
        self.start_actor_locked(&mut lc, alias, true)
    }

    /// Whether an operator paused `alias`: disabled, and its newest stop
    /// marker is not the idle timer's. A fresh registration is enabled, a
    /// timer stop stays eligible for work-driven resume, and an explicit
    /// resume re-enables the row, so a historical manual marker never blocks it.
    pub(super) fn assistant_stopped_by_operator(&self, alias: &str) -> Result<bool> {
        if self.store.agent(alias)?.enabled {
            return Ok(false);
        }
        let marker = self
            .store
            .last_event_of(alias, crate::store::AUTO_STOP_MARKER_KINDS)?;
        Ok(!marker.is_some_and(|m| m.kind == crate::store::AUTO_STOP_EVENT))
    }

    /// The member assistant's resume (CAD-1380): the inbox, fence and start
    /// path of `try_resume`, with the operator-stop check and the start claim
    /// under one lifecycle lock, so a stop cannot land between them. Operator
    /// resume and unfence keep `try_resume`. An owned alias is a live actor
    /// that drains the queue; a stop in flight refuses.
    pub(super) fn try_resume_assistant(self: &Arc<Self>, alias: &str) -> Result<()> {
        let agent = self.store.agent(alias)?;
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is an inbox — nothing to resume"
            )));
        }
        let mut lc = self.lifecycle.lock().unwrap();
        if lc.stopping.contains(alias) {
            return Err(Error::rejected(format!(
                "assistant '{alias}' is stopping — the message stays queued"
            )));
        }
        if lc.agents.contains_key(alias) {
            // CAD-1380: the owned no-op resume still gates on the cwd invariant.
            self.assistant_start_guard(alias)?;
            return Ok(());
        }
        if self.store.has_unknown(alias)? {
            return Err(Error::rejected(format!(
                "assistant '{alias}' is fenced by an unreconciled unknown message — \
                 the operator reconciles before it runs again"
            )));
        }
        if self.assistant_stopped_by_operator(alias)? {
            return Err(Error::rejected(format!(
                "assistant '{alias}' was stopped by an operator and stays stopped — \
                 the operator resumes it; the message stays queued"
            )));
        }
        if !self.start_actor_locked(&mut lc, alias, true)? {
            return Err(Error::rejected(format!(
                "assistant '{alias}' is fenced by an unreconciled unknown message"
            )));
        }
        Ok(())
    }

    /// Bounded wait for a just-started actor's `open`: live when an
    /// endpoint is published or a live state lands, over when the
    /// actor gives up (`attention`/`stopped`/`offline`). Same 30s
    /// bound the CLI's resume polling uses. Returns `(live, state)`.
    fn wait_open_outcome(&self, alias: &str) -> (bool, String) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let agent = match self.store.agent(alias) {
                Ok(a) => a,
                Err(_) => return (false, "gone".to_string()),
            };
            let live = agent.endpoint.is_some()
                || matches!(agent.state.as_str(), "idle" | "running" | "waiting_input");
            if live {
                return (true, agent.state);
            }
            if matches!(agent.state.as_str(), "attention" | "stopped" | "offline") {
                return (false, agent.state);
            }
            if Instant::now() >= deadline {
                return (false, agent.state);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// `dead` is per endpoint kind, not "no endpoint string":
    /// attachable kinds (pty, managed-ws) are dead when registered,
    /// not operator-stopped, and holding no live endpoint; managed
    /// kinds are dead when fenced or enabled-but-unattended; inbox and
    /// fake never die. `resumable` is the question `dead` was being
    /// asked: stopped-or-dead with a saved native thread and no
    /// unreconciled unknowns fencing it.
    /// CAD-325: the slice of `agent_show` the board renders an agent row
    /// from — running messages, the parked count, queue and fence counts
    /// and the event cursor — without the whole message history.
    pub(super) fn board_view(&self, alias: &str, peer_pid: u32) -> Result<Value> {
        let messages = self.store.running_messages(alias)?;
        // CAD-1853: a routed cloud reply is a `worker_result` on the
        // invoker's ordinary alias, and this view carries running message
        // bodies. Every row asks the one cloud-row policy, or the board list
        // becomes the body reader that `agent_show` refuses to be.
        // Never the turn token: it is the credential `message result`
        // checks, and the board has no use for it.
        let mut running: Vec<Value> = Vec::with_capacity(messages.len());
        for m in &messages {
            if !self.may_read_cloud_row(
                peer_pid,
                alias,
                &m.id,
                m.sender_uid.as_deref(),
                m.recipient_uid.as_deref(),
            )? {
                continue;
            }
            let mut j = m.to_json();
            if let Some(o) = j.as_object_mut() {
                o.remove("turn_id");
            }
            running.push(j);
        }
        let parked = self.store.parked_message_count(alias)?;
        Ok(json!({
            "messages": running,
            "parked": parked,
            "queued": self.store.queued_count(alias)?,
            "unknown": self.store.unknown_messages(alias)?.len(),
            "event_cursor": self.store.event_cursor(alias)?,
        }))
    }

    pub(super) fn agent_liveness(&self, agent: &Agent) -> (bool, bool) {
        let dead = if registry::attachable(&agent.provider, &agent.endpoint_kind) {
            agent.endpoint.is_none() && agent.state != "stopped"
        } else if agent.endpoint_kind == "managed" {
            agent.state == "attention"
                || (agent.enabled && !self.lifecycle.lock().unwrap().owned(&agent.alias))
        } else {
            false
        };
        let resumable = (agent.state == "stopped" || dead)
            && agent.thread_id.as_deref().is_some_and(|t| !t.is_empty())
            && !self.store.has_unknown(&agent.alias).unwrap_or(false);
        (dead, resumable)
    }

    /// `agent unfence` — the bulk `message reconcile` plus an optional
    /// resume. Operator only, by the connection (CAD-374): the same
    /// gate as reconcile, `alias` being the target; `by` is refused.
    pub(super) fn rpc_unfence(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection_on_agent("agent unfence", params, peer_pid)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        // CAD-2207: an operator must not fabricate or route a member
        // assistant's unknown outcomes — the bulk path bypasses the
        // `message_reconcile` guard. No authorized owner recovery channel
        // exists yet, so refuse before any mutation.
        if self.store.assistant_for_alias(&alias)?.is_some() {
            return Err(Error::rejected(
                "agent unfence refused: a member assistant's unknown outcomes are private \
                 to its owning member; no operator recovery channel is authorized \
                 (CAD-2207)",
            ));
        }
        let status = required_str(params, "status")?;
        let note = optional_str(params, "note");
        let by = "operator";
        // Resolve the agent before any reconcile so a bad alias fails
        // without side effects.
        let agent = self.store.agent(&alias)?;
        if agent.archived_at.is_some() {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is archived — restore it before unfencing it"
            )));
        }
        let ids = self.store.unknown_messages(&alias)?;
        if ids.is_empty() {
            return Err(Error::rejected(format!(
                "Agent '{alias}' has no unknown messages to reconcile \
                 — use `cadence agent resume {alias}`"
            )));
        }
        let mut reconciled = Vec::new();
        for id in &ids {
            let message = self.store.reconcile(id, status, note, by, None)?;
            if let Some(result) = message.result.clone() {
                self.notify_routed_target(&message, &result);
            }
            reconciled.push(id.clone());
        }
        self.notify_agent(&alias);
        // `resume` is opt-in over the socket — the CLI's `agent unfence`
        // passes it unless `--no-resume`, keeping the bare-RPC call a
        // reconcile-only operation.
        let resume = params
            .get("resume")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let pty = agent.endpoint_kind == "pty";
        let mut out = json!({"alias": alias, "reconciled": reconciled});
        if !resume {
            out["resumed"] = json!(false);
            if pty {
                out["pane"] = json!("none");
            }
            out["state"] = json!(self.store.agent(&alias)?.state);
            self.wake();
            return Ok(out);
        }
        // The settled default: reconcile then bring the agent back,
        // and say what the endpoint actually did — adopted the
        // surviving pane, respawned on the recorded session, or
        // nothing because the resume failed. A resume rejection is
        // reported, not thrown: the reconcile already committed.
        let (resumed, state) = match self.try_resume(&alias) {
            Ok(true) => self.wait_open_outcome(&alias),
            Ok(false) => (false, self.store.agent(&alias)?.state),
            Err(e) => {
                out["resumed"] = json!(false);
                if pty {
                    out["pane"] = json!("none");
                }
                out["state"] = json!(self.store.agent(&alias)?.state);
                out["error"] = json!(e.to_string());
                self.wake();
                return Ok(out);
            }
        };
        out["resumed"] = json!(resumed);
        if pty {
            let pane = if resumed {
                self.open_attach
                    .lock()
                    .unwrap()
                    .get(&alias)
                    .copied()
                    .unwrap_or("respawned")
            } else {
                "none"
            };
            out["pane"] = json!(pane);
        }
        out["state"] = json!(state);
        self.wake();
        Ok(out)
    }

    pub(super) fn rpc_stop(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        // Test seam, same shape as CADENCE_TEST_INTERRUPT_PAUSE_MS:
        // `ProviderEnv::own` never reads the process environment, so
        // only an in-process test daemon that set this name fails the
        // stop, and it fails before any mutation. Value is one alias
        // or a comma-separated list.
        if self
            .provider_env
            .own("CADENCE_TEST_STOP_FAILS")
            .is_some_and(|fail| fail.split(',').any(|name| name == alias))
        {
            return Err(Error::rejected(format!(
                "test seam: stop of '{alias}' failed"
            )));
        }
        let agent = self.store.agent(&alias)?;
        if agent.archived_at.is_some() {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is archived — restore it before stopping it"
            )));
        }
        // A sandbox runner has no host actor, so the inbox refusal below
        // does not apply. A running kickoff is stopped by the poller on a
        // durable request; a queued one is settled before any create.
        if agent.provider == "sandbox" && agent.endpoint_kind == "sandbox" {
            let mut running = 0;
            let mut queued = 0;
            for message in self.store.messages(&alias)? {
                if message.source != "job_dispatch" {
                    continue;
                }
                match message.state.as_str() {
                    "running" => {
                        self.store.request_cloud_stop(&message.id)?;
                        running += 1;
                    }
                    "queued" => {
                        self.store.request_cloud_stop(&message.id)?;
                        queued += 1;
                    }
                    "unknown" => {
                        self.store.request_cloud_stop(&message.id)?;
                        running += 1;
                        if let Some(task_id) = message.task_id.clone() {
                            let shared = Arc::clone(self);
                            let message_id = message.id.clone();
                            std::thread::spawn(move || {
                                let _ = shared.run_cloud_dispatch(&task_id, &message_id);
                            });
                        }
                    }
                    _ => {}
                }
            }
            if running == 0 && queued == 0 {
                return Err(Error::rejected(format!(
                    "Agent '{alias}' has no active cloud dispatch to stop"
                )));
            }
            // A cloud stop is per-job. The actorless agent stays enabled:
            // there is no actor/resume path to re-enable it, and a stopped
            // job must not permanently block later dispatch.
            let _ = self.store.event_public(
                &alias,
                "stop_requested",
                json!({"cloud_jobs": running, "queued_stop_requested": queued}),
            );
            return Ok(json!({"alias": alias, "state": "stopping",
                             "cloud_jobs": running, "queued_stop_requested": queued}));
        }
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is an inbox — no actor to stop; \
                 `cadence agent remove {alias}` deletes the mailbox"
            )));
        }
        // Reserve the alias for the whole stop — through the final state
        // write — so a resume cannot start a new actor in the gap where
        // the old actor already released ownership.
        let ctl = {
            let mut lc = self.lifecycle.lock().unwrap();
            // A stop already in flight owns the alias through its final
            // write; reject before any mutation rather than letting a
            // second operation write stale state over a newer actor.
            if !lc.stopping.insert(alias.clone()) {
                return Err(Error::rejected("Agent is already stopping"));
            }
            lc.agents.get(&alias).cloned()
        };
        let _reservation = StopReservation {
            lifecycle: &self.lifecycle,
            alias: &alias,
        };
        // The archive holds the same lifecycle lock. Re-read after the
        // reservation so an archive that won the lock first refuses here.
        if self.store.agent(&alias)?.archived_at.is_some() {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is archived — restore it before stopping it"
            )));
        }
        self.store.set_enabled(&alias, false)?;
        let _ = self.store.event_public(&alias, "stop_requested", json!({}));
        // A fenced agent keeps its attention state and reason; the stop
        // only disables it.
        if self.store.agent(&alias)?.state != "attention" {
            self.store.set_agent_state(&alias, "stopping", None)?;
        }
        // CAD-201: the pane root identity is read while the agent row
        // still names its live generation — the actor's exit clears it.
        let pane_tree =
            (agent.endpoint_kind == "pty").then(|| self.owned_pane_root(&alias, &agent));
        if let Some(ctl) = ctl {
            self.stop_ctls(&[ctl]);
        }
        // A fenced pty agent's pane survived the fence for inspection —
        // `agent stop` is the explicit kill. For a live agent the
        // actor's own close() already ran, so this is a no-op for it.
        if agent.endpoint_kind == "pty" {
            adapter::pty::kill_pane(&self.state_dir, &alias, &self.provider_env, self.agent_uid);
        }
        match pane_tree {
            Some(Ok(Some(root))) => {
                // The drain is bounded but long (60s by default) — it
                // runs on its own thread, never in this RPC or an actor.
                let shared = Arc::clone(self);
                let owned = alias.clone();
                thread::spawn(move || shared.reap_pane_tree(&owned, &root));
            }
            Some(Err(reason)) => {
                let _ = self.store.event_public(
                    &alias,
                    "pane_tree_unowned",
                    json!({"reason": reason,
                           "note": "no process was signalled beyond the pane itself"}),
                );
            }
            // Not pty, or this tree was already reaped.
            Some(Ok(None)) | None => {}
        }
        // The actor writes its own terminal state on exit; do not mask a
        // fence it may have raised while finishing.
        let state = if self.store.agent(&alias)?.state == "attention" {
            "attention"
        } else {
            // One write: `stopped` lands with the runtime fields
            // cleared — the actor may still be finishing its own exit.
            self.store.set_state_detached(&alias, "stopped", None)?;
            "stopped"
        };
        self.wake();
        Ok(json!({"alias": alias, "state": state}))
    }

    /// CAD-201: the pane-root identity `agent stop` may reap by —
    /// the newest `pane_root` record, provided it belongs to the
    /// endpoint generation the agent row still names (when it names
    /// one). `Ok(None)`: the newest record is already a reap result,
    /// so a repeated stop signals nothing. `Err`: the tree is unowned
    /// — nothing recorded (an agent opened before CAD-201), an
    /// unreadable root, or a stale generation — and nothing is
    /// signalled.
    fn owned_pane_root(
        &self,
        alias: &str,
        agent: &Agent,
    ) -> std::result::Result<Option<adapter::pty::lane::PaneRoot>, String> {
        let latest = self
            .store
            .last_event_of(alias, PANE_TREE_KINDS)
            .map_err(|e| format!("pane root record unreadable: {e}"))?;
        let Some(event) = latest else {
            return Err("no pane root identity was recorded for this agent \
                        (its pane was opened before CAD-201)"
                .to_string());
        };
        match event.kind.as_str() {
            "pane_root" => {}
            "pane_root_unrecorded" => {
                return Err("the pane root's identity was unreadable at open".to_string())
            }
            _ => return Ok(None),
        }
        let root = adapter::pty::lane::PaneRoot::from_json(&event.payload)
            .ok_or_else(|| "the recorded pane root identity is malformed".to_string())?;
        if let Some(current) = agent.generation.as_deref() {
            if current != root.generation {
                return Err(format!(
                    "the recorded pane root belongs to generation {}, the endpoint \
                     is at {current}",
                    root.generation
                ));
            }
        }
        Ok(Some(root))
    }

    /// This daemon's pty retry base: its provider env (a test) or the
    /// environment's `CADENCE_PTY_RETRY_SECS`, else the default. An
    /// invalid value warns on stderr and keeps the default.
    pub(super) fn pty_retry_base(&self) -> Duration {
        let raw = self.provider_env.var("CADENCE_PTY_RETRY_SECS");
        parse_pty_retry_base(raw.as_deref()).unwrap_or_else(|reason| {
            eprintln!("pty retry: {reason}; using the default {PTY_RETRY_BASE:?}");
            PTY_RETRY_BASE
        })
    }

    /// CAD-201: reap what is left of a stopped pane's session — off
    /// the actor loop and the RPC thread. Intent, result and residue
    /// land as `pane_tree_reap_intent` / `pane_tree_reaped` (or
    /// `pane_tree_reap_refused`) events on the agent's stream. The
    /// action-time check re-reads the newest pane record: a reopen
    /// whose root sits on the recorded session id stops the reap.
    fn reap_pane_tree(&self, alias: &str, root: &adapter::pty::lane::PaneRoot) {
        use adapter::pty::lane;
        let drain = self
            .provider_env
            .var("CADENCE_PTY_DRAIN_SECS")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or(lane::DEFAULT_DRAIN);
        let opts = lane::ReapOptions {
            drain,
            ..lane::ReapOptions::default()
        };
        let still_ours = || -> std::result::Result<(), String> {
            let latest = self
                .store
                .last_event_of(alias, &["pane_root"])
                .map_err(|e| format!("pane root record unreadable at action time: {e}"))?;
            match latest.and_then(|e| lane::PaneRoot::from_json(&e.payload)) {
                Some(newer) if newer != *root && newer.sid == root.sid => Err(format!(
                    "a newer endpoint (generation {}) recorded a pane root on the \
                     same session id {}",
                    newer.generation, root.sid
                )),
                _ => Ok(()),
            }
        };
        let on_intent = |members: &[lane::Member]| {
            let members: Vec<Value> = members
                .iter()
                .map(|m| json!({"pid": m.pid, "start_time": m.start_time}))
                .collect();
            let _ = self.store.event_public(
                alias,
                "pane_tree_reap_intent",
                json!({"root": root.to_json(), "members": members,
                       "drain_secs": opts.drain.as_secs_f64()}),
            );
        };
        let helper = self
            .agent_uid
            .map(|_| adapter::pty::agent_exec_path(&self.state_dir, &self.provider_env));
        let report =
            lane::reap_session_with(root, &opts, &still_ours, &on_intent, helper.as_deref());
        let kind = report.event_kind();
        let mut payload = report.to_json();
        payload["root"] = root.to_json();
        let _ = self.store.event_public(alias, kind, payload);
    }

    /// CAD-201/CAD-202 facts for `agent show`: the recorded pane root
    /// (and whether it is the live generation's), and — while the
    /// endpoint is live — the pane's cwd with `cwd_deleted`. The cwd
    /// is read only when the row's pid is still the recorded root
    /// process (same start time), never from a reused pid.
    pub(super) fn pty_lane_facts(&self, agent: &Agent, j: &mut Value) {
        use adapter::pty::lane;
        let root = self
            .store
            .last_event_of(&agent.alias, &["pane_root"])
            .ok()
            .flatten()
            .and_then(|e| lane::PaneRoot::from_json(&e.payload));
        j["pane_root"] = match &root {
            Some(r) => {
                let mut v = r.to_json();
                v["current"] = json!(agent.generation.as_deref() == Some(r.generation.as_str()));
                v
            }
            None => Value::Null,
        };
        let live_pid = agent
            .pid
            .filter(|_| agent.endpoint.is_some())
            .and_then(|p| u32::try_from(p).ok());
        let cwd = live_pid.and_then(|pid| {
            let proven = match &root {
                Some(r) if r.pid == pid => r.check() == lane::RootState::Same,
                // No record for this pid (older open): the row's pid is
                // the live pane the actor verified — read-only use.
                _ => true,
            };
            let helper = self
                .agent_uid
                .map(|_| adapter::pty::agent_exec_path(&self.state_dir, &self.provider_env));
            proven
                .then(|| lane::pane_cwd_with(pid, helper.as_deref()))
                .flatten()
        });
        j["cwd_deleted"] = json!(cwd.as_ref().is_some_and(|c| c.deleted));
        j["pane_cwd"] = cwd.map(|c| c.to_json()).unwrap_or(Value::Null);
    }

    /// Interrupt every actor, wait one bounded grace, force-close the
    /// stragglers, then join all threads. A forced close makes any
    /// outstanding attempt `OutcomeUnknown` — fenced, never replayed.
    pub(super) fn stop_ctls(&self, ctls: &[Arc<AgentCtl>]) {
        for ctl in ctls {
            if let Some(adapter) = ctl.adapter.lock().unwrap().clone() {
                adapter.release_for_stop();
            }
            ctl.wake.notify_all();
        }
        let deadline = Instant::now() + STOP_GRACE;
        while ctls.iter().any(|c| !ctl_finished(c)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        for ctl in ctls {
            if !ctl_finished(ctl) {
                if let Some(adapter) = ctl.adapter.lock().unwrap().clone() {
                    // A forced close makes any outstanding attempt
                    // OutcomeUnknown — fenced, never replayed. During
                    // daemon shutdown the endpoint must detach, not
                    // die: owned endpoints (a pty pane) outlive the
                    // controller and are revalidated on the next open —
                    // killing one here orphans the session the restart
                    // is meant to re-adopt. A held Devin cloud turn
                    // detaches too, so its session is not archived.
                    // `detach` defaults to `close` for adapters that
                    // own their provider process, so managed endpoints
                    // are still reaped.
                    if self.closing.load(Ordering::SeqCst) || ctl.cloud_held.load(Ordering::SeqCst)
                    {
                        adapter.detach();
                    } else {
                        adapter.close();
                    }
                }
                ctl.wake.notify_all();
            }
        }
        for ctl in ctls {
            if let Some(handle) = ctl.thread.lock().unwrap().take() {
                let _ = handle.join();
            }
        }
    }

    /// Resolve a user-facing agent name to the canonical alias. Accepts
    /// an alias or a provider-native id — a Devin session slug or Codex
    /// thread id — so agents stay addressable by their native handle.
    /// Exact aliases always win over native ids.
    pub(super) fn resolve_alias(&self, name: &str) -> Result<String> {
        if let Some(agent) = self.store.agent_opt(name)? {
            return Ok(agent.alias);
        }
        self.store
            .agent_by_native(name)?
            .map(|agent| agent.alias)
            .ok_or_else(|| Error::rejected("Unknown managed agent"))
    }

    /// The agent's registered upstream (`params.upstream`), if any —
    /// used as the default `reply_to` for its sends.
    pub(super) fn upstream_of(&self, alias: &str) -> Option<String> {
        let agent = self.store.agent(alias).ok()?;
        agent
            .params
            .as_ref()?
            .get("upstream")?
            .as_str()
            .map(str::to_string)
    }

    // ---- Unconsumed inboxes: warn, never refuse or drop (CAD-251) ----

    /// The mailbox's health block ([`crate::inbox::health`]) — `None`
    /// for an agent with an actor (it consumes its own queue) or when
    /// the store read fails.
    pub(super) fn inbox_health(&self, agent: &Agent) -> Option<Value> {
        if registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return None;
        }
        let consumer = self.store.inbox_consumer(&agent.alias).ok()?;
        let policy = crate::inbox::Policy::from_params(agent.params.as_ref());
        let owner = crate::inbox::owner_of(&agent.alias, |a| self.upstream_of(a));
        Some(crate::inbox::health(
            &agent.alias,
            &consumer,
            policy,
            &owner,
            epoch_secs(),
        ))
    }

    /// The sender-facing warning for a delivery into `alias`, when it
    /// is a stale mailbox.
    pub(super) fn inbox_warning(&self, alias: &str) -> Option<String> {
        let agent = self.store.agent(alias).ok()?;
        let health = self.inbox_health(&agent)?;
        health["warning"].as_str().map(str::to_string)
    }

    /// Routed deliveries (reply_to results, upstream notices) have no
    /// CLI caller to warn, so a stale mailbox that received anything
    /// since its last warning gets one `inbox_unconsumed` event — at
    /// most once per idle window, never per message.
    pub(super) fn inbox_sweep(&self) {
        let Ok(agents) = self.store.agents() else {
            return;
        };
        let now = epoch_secs();
        for agent in &agents {
            let Some(health) = self.inbox_health(agent) else {
                continue;
            };
            if health["stale"] != json!(true) {
                continue;
            }
            let received = health["last_received_at"].as_f64().unwrap_or(0.0);
            let window = health["threshold"]["idle_secs"].as_f64().unwrap_or(0.0);
            let warned = self
                .store
                .last_event_at(&agent.alias, "inbox_unconsumed")
                .ok()
                .flatten();
            if let Some(at) = warned {
                if received <= at || now - at < window {
                    continue;
                }
            }
            let _ = self
                .store
                .event_public(&agent.alias, "inbox_unconsumed", health);
        }
    }
}

/// A concrete `ip:port` four-tuple half from a request field. Never a host
/// name: the board's bound address is an IP (see `concrete_board_server`), and
/// a name would have no one destination to match the client socket against.
fn peer_addr_param(params: &Value, key: &str) -> Result<std::net::SocketAddr> {
    required_str(params, key)?
        .parse()
        .map_err(|_| Error::rejected(format!("'{key}' must be an ip:port socket address")))
}

/// CAD-2207: the ordinary carried-stale pairs a fleet-state caller asks the
/// daemon to exclude. Strict: every row is a two-string `[alias, message_id]`
/// pair; a malformed row refuses rather than silently widening the gate.
fn ignored_stale_pairs(params: &Value) -> Result<Vec<(String, String)>> {
    let Some(value) = params.get("ignored_stale") else {
        return Ok(Vec::new());
    };
    let rows = value
        .as_array()
        .ok_or_else(|| Error::rejected("ignored_stale must be an array"))?;
    let mut out = Vec::new();
    for row in rows {
        let pair = row
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or_else(|| Error::rejected("ignored_stale rows must be [alias, message_id]"))?;
        let alias = pair[0]
            .as_str()
            .ok_or_else(|| Error::rejected("ignored_stale alias must be a string"))?;
        let id = pair[1]
            .as_str()
            .ok_or_else(|| Error::rejected("ignored_stale message id must be a string"))?;
        out.push((alias.to_string(), id.to_string()));
    }
    Ok(out)
}

/// CAD-2207: strict recovery-record rows for the drain aggregate. A
/// missing/malformed array or row returns `None` (unverified); the
/// aliases are used only to decide emptiness and are never returned.
fn recovery_rows(value: Option<&Value>) -> Option<Vec<(String, String)>> {
    let rows = value?.as_array()?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let obj = row.as_object()?;
        let alias = obj.get("alias")?.as_str()?;
        let message_id = obj.get("message_id")?.as_str()?;
        if alias.is_empty() || message_id.is_empty() {
            return None;
        }
        out.push((alias.to_string(), message_id.to_string()));
    }
    Some(out)
}
