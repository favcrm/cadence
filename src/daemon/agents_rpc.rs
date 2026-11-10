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

    /// The visible agent-event page size. An operator's numeric page keeps its
    /// existing bound; a scoped page is filled with visible rows only.
    const EVENTS_PAGE: usize = 100;
    /// The newest scoped tail page.
    const EVENTS_TAIL_PAGE: usize = 50;
    /// One raw store window of a scoped scan. A page continues past rows the
    /// reader may not see, so a window may contribute nothing.
    const EVENTS_SCAN_CHUNK: i64 = 100;

    pub(super) fn rpc_events(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        // CAD-2215: the request shape is settled BEFORE any agent is named or
        // any event is read, so a downgrade attempt learns nothing.
        let format = agent_events_opaque::requested_format(params, "agent events")?;
        if format.is_some_and(|format| format != agent_events_opaque::FORMAT) {
            return Err(agent_events_opaque::format_required("agent events"));
        }
        let after = agent_events_opaque::supplied_after(params);
        let raw = required_str(params, "alias")?;
        // The daemon stream has no agents row — read-only, so a plain
        // match suffices; every other RPC keeps resolving to an agent.
        let daemon = raw == DAEMON_ALIAS;
        let alias = if daemon {
            raw.to_string()
        } else {
            self.resolve_alias(raw)?
        };
        let reader = self.verified_agent_event_reader(params, peer_pid, "agent events")?;
        if !daemon
            && self.member_agent_row_withheld_for_relay(
                &alias,
                params,
                reader.cli.as_ref(),
                reader.public.as_ref(),
                peer_pid,
            )?
        {
            return Err(Error::rejected("Unknown managed agent"));
        }
        // CAD-2215: a scoped reader must continue with a token and must name
        // the opaque format. A number is a downgrade, and an unknown/absent
        // format is refused — never coerced and never served as numeric
        // positions. A proven operator keeps the legacy numeric document.
        let scoped = reader.principal.is_some();
        // CAD-2215: a scoped cursor is bound to the reader's readable project
        // set (length-framed and sorted), not to the dynamic issue map or the
        // private-job denial rows. A withdrawal or grant changes the
        // authorization set, so a token minted under the old set refuses and
        // resumes when the same set is restored; ordinary work and hidden
        // event/issue insertion never move it. A genuinely projectless reader
        // still gets an empty page.
        let filters = if scoped {
            Some(reader.readable_filters())
        } else {
            None
        };
        let supplied = if scoped {
            if format.is_none() {
                return Err(agent_events_opaque::format_required("agent events"));
            }
            match after {
                agent_events_opaque::SuppliedAfter::Absent => None,
                agent_events_opaque::SuppliedAfter::Text(token) => Some(token),
                agent_events_opaque::SuppliedAfter::Number => {
                    return Err(agent_events_opaque::numeric_cursor("agent events"))
                }
                agent_events_opaque::SuppliedAfter::Unsupported => {
                    return Err(agent_events_opaque::invalid_after("agent events"))
                }
            }
        } else {
            // The legacy numeric continuation, preserved exactly for the
            // operator. An opaque token, a non-integer number or any other
            // shape is not a position, and is never coerced to zero.
            match after {
                agent_events_opaque::SuppliedAfter::Absent => {}
                agent_events_opaque::SuppliedAfter::Number => {
                    if params.get("after").and_then(Value::as_i64).is_none() {
                        return Err(agent_events_opaque::invalid_after("agent events"));
                    }
                }
                agent_events_opaque::SuppliedAfter::Text(_)
                | agent_events_opaque::SuppliedAfter::Unsupported => {
                    return Err(agent_events_opaque::invalid_after("agent events"))
                }
            }
            None
        };
        let numeric_after = params.get("after").and_then(Value::as_i64).unwrap_or(0);
        if numeric_after < 0 {
            return Err(Error::rejected("Event cursor must be nonnegative"));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        // Missing key material refuses a scoped read before any content; it
        // never downgrades to numeric positions. The stream identity (durable
        // `uid` for a registered agent, a fixed resource for the daemon) is
        // bound into every token.
        let token_material = if scoped {
            Some((
                self.scoped_cursor_codec()?,
                reader.principal.clone().ok_or_else(|| {
                    Error::rejected("agent events refused: reader access changed")
                })?,
                self.agent_event_stream(&alias, daemon, "agent events")?,
            ))
        } else {
            None
        };
        let position = match (token_material.as_ref(), supplied) {
            (Some((codec, principal, stream)), Some(token)) => Some(agent_events_opaque::open(
                codec,
                principal,
                stream,
                filters.as_deref().unwrap_or_default(),
                token,
                "agent events",
            )?),
            _ => None,
        };
        let private_denials = if reader.scope.is_some() {
            self.private_job_denials_for_read(
                params,
                reader.cli.as_ref(),
                reader.public.as_ref(),
                peer_pid,
            )?
        } else {
            Default::default()
        };
        let allowed_issues = reader.scope.as_ref().map(|(readable, issues)| {
            issues
                .iter()
                .filter(|(_, holders)| holders.iter().any(|project| readable.contains(project)))
                .map(|(issue, _)| issue.as_str())
                .collect::<Vec<_>>()
        });
        // CAD-2215: the alias each row's `message` binding resolved to when
        // that row was SELECTED. The post-selection cloud reproof re-uses that
        // attribution instead of re-reading the message table, so a row is
        // always judged against the alias its admission was proven for and a
        // message that disappears after selection cannot re-point it.
        let selected_message_alias: std::cell::RefCell<std::collections::HashMap<String, String>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
        let revalidate = |selected: &[crate::store::Event]| -> Result<()> {
            let current = self.verified_agent_event_reader(params, peer_pid, "agent events")?;
            let changed = || Error::rejected("agent events refused: reader access changed");
            if !reader.same_authority(&current) {
                return Err(changed());
            }
            // The durable stream identity is re-read after selection: a
            // re-registered alias (new uid) can never inherit this page.
            if let Some((_, _, stream)) = token_material.as_ref() {
                let current_stream = self
                    .agent_event_stream(&alias, daemon, "agent events")
                    .map_err(|_| changed())?;
                if !stream.same_identity(&current_stream) {
                    return Err(changed());
                }
            }
            if !daemon
                && self.member_agent_row_withheld_for_relay(
                    &alias,
                    params,
                    current.cli.as_ref(),
                    current.public.as_ref(),
                    peer_pid,
                )?
            {
                return Err(changed());
            }
            // CAD-2215: re-admit exactly the rows this page returns against
            // the CURRENT task/job/message -> issue/project/denial bindings.
            // An issue can move, or a row's task/job can become denied, while
            // the readable project set stays the same. Comparing whole
            // issue/denial sets instead would make an unrelated hidden
            // insertion refuse the read (an activity oracle).
            if current.scope.is_some() {
                self.reprove_selected_agent_rows(&alias, selected, &current, params, peer_pid)?;
                // CAD-2215: the project/SQL reproof above cannot see a cloud
                // grant revocation: the snapshot this page was selected under
                // still looks current for an already-KNOWN message, so the
                // stale projection would release cloud content whose
                // authority was withdrawn. Re-prove the same admission
                // predicate now, per alias the page actually returns.
                self.reprove_selected_cloud_rows(selected, &selected_message_alias, peer_pid)?;
            }
            Ok(())
        };
        // CAD-1853 review: the alias' event stream carries cloud request and
        // result detail, so it applies the same source/owner decision as the
        // dedicated cloud read before projecting anything — current READ
        // authority, and only the caller's own source's rows.
        let mut projection = self.admit_cloud_projection(&alias, peer_pid, "agent events")?;
        // CAD-1380: daemon `assist_intent` rows are member-private routing. Only
        // the operator reads them from the shared stream; every other caller gets
        // the same page with those rows removed, cursors unchanged.
        let hide_intents = daemon && scoped;
        let visible = |projection: &cloud_agent_rpc::CloudProjection,
                       events: Vec<crate::store::Event>|
         -> Result<Vec<crate::store::Event>> {
            let mut foreign = std::collections::HashMap::new();
            let mut visible = Vec::with_capacity(events.len());
            for mut event in events {
                if hide_intents && event.kind == "assist_intent" {
                    continue;
                }
                // A dispatch event can name a message delivered to a different
                // agent. Prove cloud authority against that message's actual
                // alias rather than treating absence from this alias as proof.
                let selected: &cloud_agent_rpc::CloudProjection = if let Some(id) =
                    event.payload.get("message").and_then(Value::as_str)
                {
                    let Some(message) = self.store.message(id)? else {
                        continue;
                    };
                    // CAD-2215: remember the attribution this row was
                    // admitted under, so the post-selection reproof judges the
                    // same alias instead of re-deriving one.
                    selected_message_alias
                        .borrow_mut()
                        .insert(id.to_string(), message.alias.clone());
                    if message.alias != alias {
                        match foreign.entry(message.alias) {
                            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                            std::collections::hash_map::Entry::Vacant(entry) => {
                                let proof = self.admit_cloud_projection(
                                    entry.key(),
                                    peer_pid,
                                    "agent events",
                                )?;
                                entry.insert(proof)
                            }
                        }
                    } else {
                        projection
                    }
                } else {
                    projection
                };
                if !selected.admits_event(&event) {
                    continue;
                }
                if scoped {
                    agent_events_opaque::project_scoped_event(&mut event);
                }
                visible.push(event);
            }
            Ok(visible)
        };
        // `tail` is the newest-first default `cadence events` uses
        // when no --after cursor is given: one bounded page ending at
        // the latest seq, oldest first within it, plus the forward
        // cursor and whether older history exists below the page.
        if params.get("tail").and_then(Value::as_bool).unwrap_or(false) {
            // A scoped tail is the newest VISIBLE page: scan newest-first past
            // rows this reader may not see, with a real exclusive `before`
            // bound so the scan makes progress instead of re-reading the same
            // rows, and count `has_older` from visible rows only.
            let (mut events, has_older, operator_cursor) = if scoped {
                let issues = allowed_issues.as_deref().unwrap_or(&[]);
                let mut before = i64::MAX;
                let mut collected: Vec<crate::store::Event> = Vec::new();
                loop {
                    let rows = self.store.events_scoped_before(
                        &alias,
                        before,
                        Self::EVENTS_SCAN_CHUNK,
                        issues,
                        &private_denials,
                    )?;
                    if rows.is_empty() {
                        break;
                    }
                    let full = rows.len() as i64 == Self::EVENTS_SCAN_CHUNK;
                    let oldest = rows.first().map(|e| e.seq).unwrap_or(before);
                    if projection.needs_readmission(
                        &self.store,
                        rows.iter().filter_map(|event| {
                            event.payload.get("message").and_then(Value::as_str)
                        }),
                    )? {
                        projection =
                            self.admit_cloud_projection(&alias, peer_pid, "agent events")?;
                    }
                    collected.extend(visible(&projection, rows)?);
                    if collected.len() > Self::EVENTS_TAIL_PAGE {
                        break;
                    }
                    if !full {
                        break;
                    }
                    before = oldest;
                }
                collected.sort_by_key(|event| event.seq);
                let has_older = collected.len() > Self::EVENTS_TAIL_PAGE;
                if has_older {
                    // Keep one older visible row until reproof: it is the
                    // witness for has_older, even though it is not returned.
                    let page_start = collected.len() - Self::EVENTS_TAIL_PAGE - 1;
                    collected.drain(..page_start);
                }
                (collected, has_older, None)
            } else {
                let mut events = match allowed_issues.as_deref() {
                    Some(issues) => {
                        self.store
                            .events_scoped(&alias, 0, 51, issues, &private_denials, true)?
                    }
                    None => self.store.events_tail(&alias, 51)?,
                };
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
                let events = visible(&projection, events)?;
                (events, has_older, Some(cursor))
            };
            #[cfg(test)]
            if scoped {
                if let Some(hook) = &self.agent_events_selected_hook {
                    hook(&alias, &events);
                }
            }
            revalidate(&events)?;
            if scoped && has_older {
                events.remove(0);
            }
            let mut page = match token_material.as_ref() {
                Some((codec, principal, stream)) => agent_events_opaque::scoped_page(
                    codec,
                    principal,
                    stream,
                    filters.as_deref().unwrap_or_default(),
                    &events,
                    // A tail page ignores `after`; its cursor is the last
                    // visible row or null, never a re-echoed token.
                    None,
                    Some(has_older),
                    "agent events",
                )?,
                None => json!({
                    "events": events.iter().map(crate::store::Event::to_json).collect::<Vec<_>>(),
                    "cursor": operator_cursor.unwrap_or(0),
                    "has_older": has_older,
                }),
            };
            if let Some(reason) = projection
                .withheld
                .filter(|_| !private_denials.exclude_cloud)
            {
                page["cloud_withheld"] = json!(reason);
            }
            Self::mark_agent_event_reader_scope(&mut page, &reader, format.is_some());
            return Ok(page);
        }
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            if scoped {
                // Forward scan: keep reading windows until the page is full of
                // VISIBLE rows or the stream ends. Hidden rows shorten neither
                // the page nor its continuation.
                // The attribution map describes the rows of THIS attempt: an
                // earlier attempt's ids are not among the rows this page can
                // return, so keeping them would only grow the map.
                selected_message_alias.borrow_mut().clear();
                let mut scan = position.unwrap_or(0);
                let mut collected: Vec<crate::store::Event> = Vec::with_capacity(Self::EVENTS_PAGE);
                let issues = allowed_issues.as_deref().unwrap_or(&[]);
                loop {
                    let rows = self.store.events_scoped(
                        &alias,
                        scan,
                        Self::EVENTS_SCAN_CHUNK,
                        issues,
                        &private_denials,
                        false,
                    )?;
                    if rows.is_empty() {
                        break;
                    }
                    let full = rows.len() as i64 == Self::EVENTS_SCAN_CHUNK;
                    scan = rows.last().map(|e| e.seq).unwrap_or(scan);
                    if projection.needs_readmission(
                        &self.store,
                        rows.iter().filter_map(|event| {
                            event.payload.get("message").and_then(Value::as_str)
                        }),
                    )? {
                        projection =
                            self.admit_cloud_projection(&alias, peer_pid, "agent events")?;
                    }
                    let mut chunk = visible(&projection, rows)?;
                    let need = Self::EVENTS_PAGE.saturating_sub(collected.len());
                    if need == 0 || chunk.len() >= need {
                        collected.extend(chunk.drain(..need));
                        break;
                    }
                    collected.extend(chunk);
                    if !full {
                        break;
                    }
                }
                if !collected.is_empty()
                    || self.closing.load(Ordering::SeqCst)
                    || Instant::now() >= deadline
                {
                    #[cfg(test)]
                    if let Some(hook) = &self.agent_events_selected_hook {
                        hook(&alias, &collected);
                    }
                    revalidate(&collected)?;
                    let mut page = match token_material.as_ref() {
                        Some((codec, principal, stream)) => agent_events_opaque::scoped_page(
                            codec,
                            principal,
                            stream,
                            filters.as_deref().unwrap_or_default(),
                            &collected,
                            supplied,
                            None,
                            "agent events",
                        )?,
                        None => {
                            return Err(Error::rejected(
                                "agent events refused: reader access changed",
                            ))
                        }
                    };
                    if let Some(reason) = projection
                        .withheld
                        .filter(|_| !private_denials.exclude_cloud)
                    {
                        page["cloud_withheld"] = json!(reason);
                    }
                    Self::mark_agent_event_reader_scope(&mut page, &reader, format.is_some());
                    return Ok(page);
                }
            } else {
                let events = self.store.events(&alias, numeric_after, 100)?;
                if !events.is_empty()
                    || self.closing.load(Ordering::SeqCst)
                    || Instant::now() >= deadline
                {
                    // A waiting read can observe an event about a turn that
                    // settled after this admission was taken — or, on an ordinary
                    // alias, an event naming a routed cloud result that resolved
                    // since. Re-resolve the page's whole admission before
                    // projecting it, exactly as `thread_read` does: the old
                    // admission never covered those rows. This page is returned
                    // either way, so one re-resolution is the bound.
                    let cursor = events.last().map(|e| e.seq).unwrap_or(numeric_after);
                    let events = if events.is_empty() {
                        events
                    } else if projection.needs_readmission(
                        &self.store,
                        events.iter().filter_map(|event| {
                            event.payload.get("message").and_then(Value::as_str)
                        }),
                    )? {
                        projection =
                            self.admit_cloud_projection(&alias, peer_pid, "agent events")?;
                        visible(&projection, events)?
                    } else {
                        visible(&projection, events)?
                    };
                    revalidate(&events)?;
                    let mut page = json!({
                        "events": events.iter().map(crate::store::Event::to_json).collect::<Vec<_>>(),
                        "cursor": cursor,
                    });
                    if let Some(reason) = projection
                        .withheld
                        .filter(|_| !private_denials.exclude_cloud)
                    {
                        page["cloud_withheld"] = json!(reason);
                    }
                    Self::mark_agent_event_reader_scope(&mut page, &reader, format.is_some());
                    return Ok(page);
                }
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// CAD-2215: the per-response reader acknowledgement. A scoped page always
    /// names the opaque reader scope. A relayed operator page keeps the
    /// acknowledgement it has always carried; a proven operator that asked for
    /// the opaque format gets the additive operator acknowledgement instead, so
    /// no client mistakes numeric positions for opaque ones.
    fn mark_agent_event_reader_scope(
        out: &mut Value,
        reader: &agent_events_opaque::VerifiedAgentEventReader,
        format_requested: bool,
    ) {
        let field = cadence_core::board_api::READER_SCOPE_FIELD;
        if reader.principal.is_some() {
            out[field] = json!(cadence_core::board_api::AGENT_EVENTS_SCOPE_ACK);
        } else if format_requested {
            out[field] = json!(cadence_core::board_api::AGENT_EVENTS_OPERATOR_SCOPE_ACK);
        } else if reader.relayed {
            out[field] = json!(cadence_core::board_api::AGENT_EVENTS_SCOPE_ACK);
        }
    }

    /// CAD-2215: re-admit exactly the rows a scoped page is about to return.
    ///
    /// The page was selected under an earlier snapshot. An issue can be
    /// reassigned to a non-readable project (or a row's task/job can become
    /// private) while the reader's readable project SET stays the same, so a
    /// set comparison is not enough. Re-running the same authorization
    /// predicate over exactly the selected seqs (one JSON-array query), with
    /// the CURRENT issue map and denial rows, is: a selected row that is no
    /// longer admitted refuses the page, while an unrelated hidden job/issue
    /// arriving meanwhile is not selected and cannot turn the read into a
    /// refusal.
    ///
    /// This covers project/job/task/denial bindings only. Cloud admission is
    /// re-proved separately by [`Self::reprove_selected_cloud_rows`], because
    /// neither this query nor the readable project set can see a withdrawn
    /// cloud grant, credential or owner.
    fn reprove_selected_agent_rows(
        &self,
        alias: &str,
        selected: &[crate::store::Event],
        current: &agent_events_opaque::VerifiedAgentEventReader,
        params: &Value,
        peer_pid: u32,
    ) -> Result<()> {
        let Some((readable, issues)) = current.scope.as_ref() else {
            return Ok(());
        };
        if selected.is_empty() {
            return Ok(());
        }
        let changed = || Error::rejected("agent events refused: reader access changed");
        let allowed = issues
            .iter()
            .filter(|(_, holders)| holders.iter().any(|project| readable.contains(project)))
            .map(|(issue, _)| issue.as_str())
            .collect::<Vec<_>>();
        let denied = self
            .private_job_denials_for_read(
                params,
                current.cli.as_ref(),
                current.public.as_ref(),
                peer_pid,
            )
            .map_err(|_| changed())?;
        let seqs = selected.iter().map(|event| event.seq).collect::<Vec<_>>();
        // One exact selected-set read (a JSON array of at most one page of
        // seqs), not a numeric span: global seq gaps never widen the work, and
        // only `seq` is projected, so no event payload is copied.
        let admitted = self
            .store
            .events_scoped_admitted_seqs(alias, &seqs, &allowed, &denied)
            .map_err(|_| changed())?;
        let present = admitted
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        for seq in &seqs {
            if !present.contains(seq) {
                return Err(changed());
            }
        }
        Ok(())
    }

    /// CAD-2215: re-prove CURRENT cloud admission for exactly the rows a
    /// scoped page is about to return — including a tail's older `has_older`
    /// witness, which is still among the selected rows at this point — and,
    /// for a dispatch row naming a message delivered elsewhere, against that
    /// message's own alias rather than this stream's.
    ///
    /// The projection a page was selected under is a snapshot, and
    /// [`cloud_agent_rpc::CloudProjection::needs_readmission`] only notices
    /// ids that snapshot never named, or a request-only operation that has
    /// since settled. A cloud grant — or the credential, enrollment scope or
    /// logical owner behind it — revoked for an ALREADY KNOWN message leaves
    /// that snapshot looking current, so the row would be released from it.
    /// Re-resolving the same admission predicate now, once per alias this page
    /// actually returns rows from, closes that window: a selected row the
    /// fresh admission no longer admits refuses the page instead of being
    /// returned.
    ///
    /// Only selected rows and their aliases are re-proved. An unrelated
    /// hidden insertion, a revocation on an alias the page does not name, and
    /// a row that was already withheld at selection time cannot turn this read
    /// into a refusal, so the refusal never reports work the reader may not
    /// see.
    fn reprove_selected_cloud_rows(
        &self,
        selected: &[crate::store::Event],
        selected_message_alias: &std::cell::RefCell<std::collections::HashMap<String, String>>,
        peer_pid: u32,
    ) -> Result<()> {
        if selected.is_empty() {
            return Ok(());
        }
        let changed = || Error::rejected("agent events refused: reader access changed");
        let attribution = selected_message_alias.borrow();
        // At most one fresh admission per alias, and only for aliases this
        // page returns rows from.
        let mut proofs: std::collections::HashMap<String, cloud_agent_rpc::CloudProjection> =
            std::collections::HashMap::new();
        for event in selected {
            let Some(id) = event.payload.get("message").and_then(Value::as_str) else {
                // Alias-level metadata carries no request or result text, so
                // `admits_event` admits it under every admission.
                continue;
            };
            // The row was admitted through this message's alias. Without that
            // attribution it cannot be re-proved at all, so it is not served.
            let Some(row_alias) = attribution.get(id).cloned() else {
                return Err(changed());
            };
            let projection = match proofs.entry(row_alias) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let proof = self
                        .admit_cloud_projection(entry.key(), peer_pid, "agent events")
                        .map_err(|_| changed())?;
                    entry.insert(proof)
                }
            };
            if !projection.admits_event(event) {
                return Err(changed());
            }
        }
        Ok(())
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

    /// Resolve mailbox access only after the connection proves its caller.
    /// Agents may consume only their own registered mailbox.
    fn inbox_caller(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
    ) -> Result<(String, crate::peer::AgentCaller)> {
        reject_identity_fields(params, verb)?;
        let caller = self.agent_caller(peer_pid, verb)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        if let crate::peer::AgentCaller::Agent(agent) = &caller {
            if agent != &alias {
                return Err(Error::rejected(format!(
                    "{verb} refused: an agent may access only its own inbox"
                )));
            }
        }
        Ok((alias, caller))
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
    /// Both reads and consumption require a proven operator or the
    /// registered agent whose mailbox is named.
    pub(super) fn rpc_inbox(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let (alias, caller) = self.inbox_caller(params, peer_pid, "agent_inbox")?;
        let agent_uid = match &caller {
            crate::peer::AgentCaller::Agent(_) => Some(self.store.agent(&alias)?.uid),
            crate::peer::AgentCaller::Operator => None,
        };
        let reprove = || -> Result<()> {
            let changed = || Error::rejected("agent_inbox refused: caller identity changed");
            let current = self
                .agent_caller(peer_pid, "agent_inbox")
                .map_err(|_| changed())?;
            if current != caller {
                return Err(changed());
            }
            if let Some(expected_uid) = &agent_uid {
                if self
                    .store
                    .agent(&alias)
                    .map_or(true, |agent| agent.uid != *expected_uid)
                {
                    return Err(changed());
                }
            }
            Ok(())
        };
        let peek = params.get("peek").and_then(Value::as_bool) == Some(true);
        let reader = inbox_reader(params)?;
        let after = optional_i64(params, "after");
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            // A long-poll must not keep authority that expired while waiting.
            reprove()?;
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
            if peek {
                reprove()?;
            }
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
    /// cursor (`inbox_ack_reset`, operator only). An unattributed caller
    /// cannot consume, park or report on a mailbox.
    ///
    /// The ack is a watermark: `through` (or the greatest `seqs` entry)
    /// completes every still-queued message at or below it, idempotent
    /// on the `queued` state guard, and records the reader's durable
    /// cursor on the `inbox_ack` event. The store clamps `through` to
    /// the inbox's tail before recording it — a claim past the tail
    /// completes what exists but cannot blind the reader to later
    /// arrivals.
    pub(super) fn rpc_inbox_ack(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let (alias, who) = self.inbox_caller(params, peer_pid, "agent_inbox_ack")?;
        let by = match &who {
            crate::peer::AgentCaller::Agent(caller) => caller.clone(),
            crate::peer::AgentCaller::Operator => "operator".to_string(),
        };
        let reader = inbox_reader(params)?;
        // Reset redelivers queued messages, so it requires the operator.
        if params.get("reset").and_then(Value::as_bool) == Some(true) {
            if !matches!(who, crate::peer::AgentCaller::Operator) {
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
        let mut private_deliveries = Vec::new();
        for id in &ids {
            let message = self.store.reconcile(id, status, note, by, None)?;
            if let Some(result) = message.result.clone() {
                self.notify_routed_target(&message, &result);
            }
            // CAD-2215: `agent unfence --status completed` is a real
            // completion of any kickoff it settles; record the private
            // result now instead of waiting for a verdict. The native
            // reconcile above stands either way — its private settlement
            // is reported beside it, never folded into or rolled back
            // with it.
            if message.source == "job_dispatch" && message.state == "completed" {
                let mut settlement = self.settle_private_completion(&message.id);
                if let Some(row) = settlement.as_object_mut() {
                    row.insert("message".to_string(), json!(message.id));
                }
                private_deliveries.push(settlement);
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
        // Each settled kickoff's private outcome is part of the answer:
        // `reconciled` alone must not read as a private success when the
        // ledger was unavailable, and nothing here rolls back the
        // committed native reconciliation.
        if !private_deliveries.is_empty() {
            out["private_deliveries"] = json!(private_deliveries);
        }
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
