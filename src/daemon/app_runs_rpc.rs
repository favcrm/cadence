//! Operator-owned local app lifecycle and turn-bound dependency artifacts.
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_runs::{self, LocalRunProvenance, LocalRunRequest, LocalWorkflow};
use std::collections::BTreeMap;

/// Resolve app, context and per-run content in that order. A blank prompt
/// field is a reset request, so it cannot erase a saved or app default.
fn freeze_content_inputs(
    template: &crate::issue::workflow::Template,
    supplied: BTreeMap<String, String>,
    context: Option<&crate::store::app_contexts::ContextConfig>,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut effective: BTreeMap<String, String> = template
        .inputs
        .iter()
        .filter_map(|(key, spec)| {
            spec.default
                .as_ref()
                .map(|value| (key.clone(), value.clone()))
        })
        .collect();
    if let Some(context) = context {
        effective.extend(
            context
                .input_defaults
                .iter()
                .filter(|(key, _)| template.inputs.contains_key(*key))
                .filter(|(key, value)| !value.is_empty() || template.inputs[*key].default.is_none())
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    let origins = template
        .inputs
        .iter()
        .filter(|(_, spec)| spec.default.is_some())
        .map(|(key, _)| {
            let origin = if supplied.get(key).is_some_and(|value| !value.is_empty()) {
                "run_override"
            } else if context.is_some_and(|config| {
                config
                    .input_defaults
                    .get(key)
                    .is_some_and(|value| !value.is_empty())
            }) {
                "context_default"
            } else {
                "app_default"
            };
            (key.clone(), origin.to_string())
        })
        .collect();
    for (key, value) in supplied {
        if !value.is_empty()
            || template
                .inputs
                .get(&key)
                .is_none_or(|spec| spec.default.is_none())
        {
            effective.insert(key, value);
        }
    }
    (effective, origins)
}

/// The installed bundle checks an installation approval rests on: a
/// typed manifest with no legacy untyped connection slots and at least one
/// supported local workflow. Shared by the explicit operator approval and
/// the consent an operator install or update records (CAD-1119).
pub(super) fn local_execution_contract(
    files: &BTreeMap<String, String>,
) -> Result<crate::issue::app::Manifest> {
    let manifest = crate::issue::app::parse_manifest(
        files
            .get("app.md")
            .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
    )?;
    if !manifest.connections.is_empty() {
        return Err(Error::rejected(
            "local text execution does not support connection capabilities",
        ));
    }
    let mut count = 0;
    for (name, text) in files {
        if name.starts_with("workflows/") {
            LocalWorkflow::validate_template(text)?;
            count += 1;
        }
    }
    if count == 0 {
        return Err(Error::rejected(
            "installed app has no supported local workflow",
        ));
    }
    Ok(manifest)
}

impl Shared {
    /// CAD-1123 HP2: create, approve and dispatch in one operator call. The
    /// operator proof was taken by the caller. The approval is the daemon's
    /// own: it binds exactly the snapshot digest this call just created.
    fn start_app_run(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let install = required_str(params, "install_id")?;
        let team = super::app_teams_rpc::team_of(&self.state_dir, install)?.ok_or_else(|| {
            Error::rejected("installation has no default team; set one in Settings")
        })?;
        let expected = params
            .get("expected_quotes")
            .filter(|value| value.is_object())
            .ok_or_else(|| Error::rejected("expected_quotes is required"))?;
        if params.get("inputs").is_some_and(|inputs| {
            inputs
                .as_object()
                .is_none_or(|map| map.values().any(|v| !v.is_string()))
        }) {
            return Err(Error::rejected("inputs must be a string map"));
        }
        let created = self.create_app_run(params, Some(&team), Some(expected))?;
        let id = created["id"]
            .as_str()
            .ok_or_else(|| Error::internal("created run has no id"))?
            .to_string();
        let mut run = created;
        let mut replayed = run["state"] != "awaiting_approval";
        if !replayed {
            let digest = required_str(&run, "snapshot_digest")?.to_string();
            let approved = self.with_app_run_current(&id, |bundle| {
                self.store
                    .app_run_decide(&id, Some(&digest), false, Some(bundle))
            });
            // A concurrent start of the same request may have approved first.
            run = match approved {
                Ok(value) => value,
                Err(error) => {
                    let shown = self.store.app_run_show(&id)?;
                    if !matches!(shown["state"].as_str(), Some("approved" | "running")) {
                        return Err(error);
                    }
                    replayed = true;
                    shown
                }
            };
        }
        if matches!(run["state"].as_str(), Some("approved" | "running")) {
            run = self.dispatch_started_run(&id, replayed)?;
            self.auto_resume_tick();
        }
        Ok(run)
    }

    /// Dispatch the run `app_run_start` just created or approved. A replay
    /// found the run already approved or running, so a worker (or a cancel)
    /// may finish it between that read and this dispatch; the dispatch then
    /// sees a terminal run and refuses it as "approval absent or stale".
    /// For a replay that is the answer the caller would have got a moment
    /// later, so return the run as it now stands. A first start that is
    /// refused still fails.
    fn dispatch_started_run(&self, id: &str, replayed: bool) -> Result<Value> {
        match self.dispatch_app_run(id) {
            Err(error) if replayed => {
                let shown = self.store.app_run_show(id)?;
                match shown["state"].as_str() {
                    Some("succeeded" | "failed" | "cancelled") => Ok(shown),
                    _ => Err(error),
                }
            }
            other => other,
        }
    }

    /// The one run-creation path: `app_run_create` (owner and workers named by
    /// the caller) and `app_run_start` (owner and workers from the install's
    /// default team, quotes checked against `expected`). Operator proof is the
    /// caller's job.
    pub(super) fn create_app_run(
        &self,
        params: &Value,
        team: Option<&super::app_teams_rpc::Team>,
        expected: Option<&Value>,
    ) -> Result<Value> {
        // CAD-1171: a host-execution run names no owner PM — the operator's
        // own click executes its capability steps. An agent run still needs
        // one, from the caller or the install's default team.
        let owner_pm = match team {
            Some(team) => Some(team.owner_pm.clone()),
            None => optional_str(params, "owner_pm").map(str::to_string),
        };
        if params.get("source_receipt_id").is_some() != params.get("selected_post_id").is_some() {
            return Err(Error::rejected(
                "source receipt and selected post must be supplied together",
            ));
        }
        let id = required_str(params, "install_id")?;
        let name = required_str(params, "workflow")?;
        if !crate::issue::model::valid_tag(name) {
            return Err(Error::rejected("invalid installed workflow name"));
        }
        let mut inputs: BTreeMap<String, String> =
            serde_json::from_value(params.get("inputs").cloned().unwrap_or(json!({})))
                .map_err(|_| Error::rejected("inputs must be a string map"))?;
        let supplied_source = inputs.get("source").cloned();
        if serde_json::to_vec(&inputs)
            .map_err(|e| Error::internal(e.to_string()))?
            .len()
            > 32 * 1024
        {
            return Err(Error::rejected("inputs exceed encoded byte limit"));
        }
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, id, |row, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let text = files
                .get(&format!("workflows/{name}.md"))
                .ok_or_else(|| Error::rejected("workflow is not in this installed bundle"))?;
            let manifest = crate::issue::app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?;
            if manifest.app == "social-content" {
                if name == "image-instagram" && params.get("source_receipt_id").is_none() {
                    return Err(Error::rejected(
                        "Instagram image run needs a selected source receipt",
                    ));
                }
                if name == "image-manual" && params.get("source_receipt_id").is_some() {
                    return Err(Error::rejected(
                        "manual image run takes operator-pasted facts, not a receipt",
                    ));
                }
            }
            let template = crate::issue::workflow::parse_template(text)
                .map_err(|_| Error::rejected("installed workflow declarations refused"))?;
            if let Some(team) = team {
                // The team is the only source of worker aliases: a
                // caller-supplied value for a team role is a forgery.
                for (role, alias) in &team.roles {
                    if !template.inputs.contains_key(role) {
                        continue;
                    }
                    if inputs.contains_key(role) {
                        return Err(Error::rejected(
                            "team roles come from the installation team, not the request",
                        ));
                    }
                    inputs.insert(role.clone(), alias.clone());
                }
            }
            let context = optional_str(params, "context_id")
                .map(|context| self.store.app_context_proof(id, context))
                .transpose()?;
            if let Some((config, _)) = &context {
                super::app_contexts_rpc::validate_defaults(files, &config.input_defaults)?;
                let defaults: BTreeMap<_, _> = config
                    .input_defaults
                    .iter()
                    .filter(|(key, _)| template.inputs.contains_key(*key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                crate::issue::workflow::check_context_defaults(text, &defaults)?;
            }
            let (resolved_inputs, input_origins) = freeze_content_inputs(
                &template,
                inputs,
                context.as_ref().map(|(config, _)| config),
            );
            inputs = resolved_inputs;
            if serde_json::to_vec(&inputs)
                .map_err(|e| Error::internal(e.to_string()))?
                .len()
                > 32 * 1024
            {
                return Err(Error::rejected(
                    "effective inputs exceed encoded byte limit",
                ));
            }
            if let (Some(receipt), Some(post)) = (
                optional_str(params, "source_receipt_id"),
                optional_str(params, "selected_post_id"),
            ) {
                let line = self.store.app_selected_source_input(
                    id,
                    context.as_ref().map(|(_, proof)| proof.id.as_str()),
                    receipt,
                    post,
                )?;
                if supplied_source.as_ref().is_some_and(|value| value != &line) {
                    return Err(Error::rejected(
                        "supplied source differs from selected provider post",
                    ));
                }
                inputs.insert("source".into(), line);
            }
            if manifest.app == "social-content"
                && matches!(name, "image-instagram" | "image-manual")
            {
                crate::platform::agenticos_external::image_plan_preflight(
                    &inputs,
                    name == "image-manual",
                )
                .map_err(Error::rejected)?;
            }
            let workflow = LocalWorkflow::parse(text, &inputs).map_err(|error| {
                if context.is_some() {
                    Error::rejected("contextual workflow inputs refused")
                } else {
                    error
                }
            })?;
            let binding = workflow
                .publication_slot
                .as_deref()
                .map(|slot| {
                    let manifest =
                        crate::issue::app::parse_manifest(files.get("app.md").ok_or_else(
                            || Error::rejected("installation manifest unavailable"),
                        )?)?;
                    if !manifest.capabilities.contains_key(slot) {
                        return Err(Error::rejected(
                            "workflow publication slot is not declared by this app",
                        ));
                    }
                    self.app_binding_live(
                        id,
                        context.as_ref().map(|(_, proof)| proof.id.as_str()),
                        slot,
                        row,
                        files,
                    )
                })
                .transpose()?
                .flatten();
            let mut capabilities = BTreeMap::new();
            let mut quotes = BTreeMap::new();
            for slot in &workflow.capability_slots {
                let manifest = crate::issue::app::parse_manifest(
                    files
                        .get("app.md")
                        .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
                )?;
                let declared = manifest.capabilities.get(slot).ok_or_else(|| {
                    Error::rejected("run capability slot is not declared by installation")
                })?;
                if !matches!(declared.effect.as_str(), "read" | "draft") {
                    return Err(Error::rejected("run capability slot must be read or draft"));
                }
                let proof = self
                    .app_binding_live(
                        id,
                        context.as_ref().map(|(_, proof)| proof.id.as_str()),
                        slot,
                        row,
                        files,
                    )?
                    .ok_or_else(|| Error::rejected("run capability binding is absent"))?;
                capabilities.insert(slot.clone(), proof);
                quotes.insert(
                    slot.clone(),
                    self.app_capability_quote(capabilities.get(slot).unwrap())?,
                );
            }
            if let Some(expected) = expected {
                // The quote the host fetched when it drew the slot must
                // be exactly the quote this run freezes (no slot, no
                // extra slot, no different price or revision).
                let frozen = json!(quotes);
                if *expected != frozen {
                    return Err(Error::rejected(
                        "price_changed: capability price changed since the host quoted it",
                    ));
                }
            }
            self.store.app_run_create_with_capabilities(
                LocalRunRequest {
                    install_id: id,
                    bundle_digest: row["digest"].as_str().unwrap(),
                    workflow: &workflow,
                    inputs: &inputs,
                    request_id: required_str(params, "request_id")?,
                    owner_pm: owner_pm.as_deref(),
                    project_link: optional_str(params, "project_link"),
                },
                context.as_ref().map(|(_, proof)| proof),
                binding.as_ref(),
                &capabilities,
                &quotes,
                LocalRunProvenance {
                    selected_source: optional_str(params, "source_receipt_id")
                        .zip(optional_str(params, "selected_post_id")),
                    input_origins: &input_origins,
                },
            )
        })
    }
}

impl Shared {
    /// CAD-1119: an operator install or update is the consent for exactly
    /// the installed digest. Called only from the operator-gated catalog
    /// RPC after the bundle is committed; never from an agent path. The
    /// runtime snapshot re-proves the digest, so a concurrent update
    /// cannot receive consent meant for another version. A bundle that
    /// fails the approval checks is installed but not approved, and the
    /// result says why.
    pub(super) fn record_install_consent(
        &self,
        pm: &crate::issue::Pm,
        install: &str,
        digest: &str,
        via: &str,
    ) -> Value {
        let outcome = workspace::with_runtime_snapshot(pm, install, |row, files| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if row["digest"].as_str() != Some(digest) {
                return Err(Error::rejected(
                    "installation changed before its consent was recorded",
                ));
            }
            let manifest = local_execution_contract(files)?;
            let capabilities = serde_json::to_value(&manifest.capabilities)
                .map_err(|e| Error::internal(e.to_string()))?;
            self.store
                .app_install_consent(install, digest, via, &capabilities)
        });
        match outcome {
            Ok(_) => json!({"recorded": true, "via": via, "digest": digest}),
            Err(error) => json!({"recorded": false, "via": via, "digest": digest,
                "reason": error.to_string()}),
        }
    }

    pub(super) fn rpc_app_local(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let allowed: &[&str] = match method {
            "app_local_install_approve" | "app_local_install_revoke" => &["install_id", "digest"],
            "app_run_create" => &[
                "install_id",
                "workflow",
                "inputs",
                "request_id",
                "owner_pm",
                "project_link",
                "context_id",
                "source_receipt_id",
                "selected_post_id",
            ],
            "app_run_start" => &[
                "install_id",
                "workflow",
                "inputs",
                "request_id",
                "context_id",
                "source_receipt_id",
                "selected_post_id",
                "expected_quotes",
            ],
            "app_run_approve" => &["run_id", "digest"],
            "app_run_cancel" | "app_run_dispatch" | "app_run_show" => &["run_id"],
            "app_run_list" => &["install_id", "context_id"],
            "app_run_artifact" => &["artifact_id", "message", "token"],
            _ => return Err(Error::rejected("unknown app lifecycle method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app lifecycle payload must be an object"))?;
        if fields.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::rejected(
                "app lifecycle payload has unsupported fields",
            ));
        }
        for field in [
            "project_link",
            "install_id",
            "context_id",
            "source_receipt_id",
            "selected_post_id",
        ] {
            if fields.get(field).is_some_and(|value| !value.is_string()) {
                return Err(Error::rejected(
                    "optional app references must be strings when present",
                ));
            }
        }
        if method == "app_run_artifact" && fields.contains_key("message") {
            let caller = self.agent_caller(peer_pid, "app dependency artifact")?;
            let AgentCaller::Agent(alias) = caller else {
                return Err(Error::rejected(
                    "dependency artifact requires its assigned worker",
                ));
            };
            let message = required_str(params, "message")?;
            let token = required_str(params, "token")?;
            if self.store.message(message)?.map(|m| m.alias) != Some(alias) {
                return Err(Error::rejected("artifact turn belongs to another worker"));
            }
            return self.app_artifact_current(
                required_str(params, "artifact_id")?,
                Some((message, token)),
            );
        }
        self.operator_connection("app local lifecycle", params, peer_pid)?;
        match method {
            "app_local_install_approve" | "app_local_install_revoke" => {
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(
                    &pm,
                    required_str(params, "install_id")?,
                    |row, files| {
                        let _release = self
                            .app_release_lock
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let digest = required_str(params, "digest")?;
                        if row["digest"].as_str() != Some(digest) {
                            return Err(Error::rejected("installation digest is stale"));
                        }
                        if method == "app_local_install_approve" {
                            local_execution_contract(files)?;
                        }
                        self.store.app_capability_decide(
                            required_str(params, "install_id")?,
                            digest,
                            method == "app_local_install_approve",
                        )
                    },
                )
            }
            "app_run_create" => self.create_app_run(params, None, None),
            "app_run_start" => self.start_app_run(params),
            "app_run_approve" => {
                let id = required_str(params, "run_id")?;
                self.with_app_run_current(id, |current_bundle| {
                    self.store.app_run_decide(
                        id,
                        Some(required_str(params, "digest")?),
                        false,
                        Some(current_bundle),
                    )
                })
            }
            "app_run_cancel" => {
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.store
                    .app_run_decide(required_str(params, "run_id")?, None, true, None)
            }
            "app_run_dispatch" => {
                let run = self.dispatch_app_run(required_str(params, "run_id")?)?;
                // CAD-1120: a kickoff queued for a worker the idle timer
                // parked wakes it now, through the CAD-413 auto-resume
                // (which re-checks that the timer's stop is still the
                // newest); the stall watch does the same for later steps.
                self.auto_resume_tick();
                Ok(run)
            }
            "app_run_show" => self.store.app_run_show(required_str(params, "run_id")?),
            "app_run_list" => self.store.app_run_list_filtered(
                optional_str(params, "install_id"),
                optional_str(params, "context_id"),
            ),
            "app_run_artifact" => {
                if fields.contains_key("token") {
                    return Err(Error::rejected("token requires its assigned message"));
                }
                self.store
                    .app_artifact_for_operator(required_str(params, "artifact_id")?)
            }
            _ => Err(Error::rejected("unknown app lifecycle method")),
        }
    }
    pub(super) fn with_app_run_current<T>(
        &self,
        id: &str,
        callback: impl FnOnce(&str) -> Result<T>,
    ) -> Result<T> {
        let run = self.store.app_run_show(id)?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, run["install_id"].as_str().unwrap(), |row, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.app_run_binding_current(&run, row, files)?;
            callback(row["digest"].as_str().unwrap())
        })
    }
    pub(super) fn app_run_binding_current(
        &self,
        run: &Value,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Result<()> {
        if let Some(binding) = run["snapshot"]["publication"]
            .get("binding")
            .filter(|value| !value.is_null())
        {
            let binding: crate::store::app_bindings::BindingProof =
                serde_json::from_value(binding.clone())
                    .map_err(|_| Error::rejected("run binding receipt is invalid"))?;
            self.app_binding_receipt_current(
                required_str(run, "install_id")?,
                run["context_id"].as_str(),
                required_str(&run["snapshot"]["publication"], "slot")?,
                &binding,
                bundle,
                files,
            )?;
        }
        if let Some(capabilities) = run["snapshot"]["capabilities"].as_object() {
            for (slot, value) in capabilities {
                let proof: crate::store::app_bindings::BindingProof =
                    serde_json::from_value(value.clone()).map_err(|_| {
                        Error::rejected("run capability binding receipt is invalid")
                    })?;
                self.app_binding_receipt_current(
                    required_str(run, "install_id")?,
                    run["context_id"].as_str(),
                    slot,
                    &proof,
                    bundle,
                    files,
                )?;
                let quote = self.app_capability_quote(&proof)?;
                if run["snapshot"]["quotes"][slot] != json!(quote) {
                    return Err(Error::rejected(
                        "capability price changed since run creation",
                    ));
                }
            }
        }
        Ok(())
    }
    pub(super) fn dispatch_app_run(&self, id: &str) -> Result<Value> {
        let result =
            self.with_app_run_current(id, |digest| self.store.app_run_dispatch(id, digest))?;
        if result["snapshot"]["workflow"]["execution"].as_str() == Some("host") {
            // CAD-1171: the operator's own click executes the capability;
            // no worker is kicked and no message is created.
            self.execute_host_run(&result)?;
            return self.store.app_run_show(id);
        }
        for step in result["snapshot"]["workflow"]["steps"].as_array().unwrap() {
            self.notify_agent(step["assignee"].as_str().unwrap());
        }
        self.wake();
        Ok(result)
    }

    /// CAD-1171: run a host-execution run's capability slots in-process.
    /// Mirrors the worker path (`app_run_capability_call`) minus the
    /// message/turn proof: the approved, dispatched step is the proof,
    /// the frozen quote is re-checked, and the provider response is
    /// retained through the same claim/record store calls.
    fn execute_host_run(&self, run: &Value) -> Result<()> {
        use crate::contract_fixture::{classify_call, Effect};
        let install = required_str(run, "install_id")?;
        let run_id = required_str(run, "id")?;
        let step_id = required_str(&run["snapshot"]["workflow"]["steps"][0], "id")?.to_string();
        let slots: Vec<String> = run["snapshot"]["workflow"]["capability_slots"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.app_run_binding_current(run, bundle, files)?;
            for slot in &slots {
                let proof = self
                    .app_binding_live(install, run["context_id"].as_str(), slot, bundle, files)?
                    .ok_or_else(|| Error::rejected("run capability binding is absent"))?;
                let quote = self.app_capability_quote(&proof)?;
                if run["snapshot"]["quotes"][slot] != json!(quote) {
                    return Err(Error::rejected(
                        "capability price changed since run approval",
                    ));
                }
                let config = &proof.config;
                let provider = required_str(config, "provider")?;
                let tool = required_str(&config["mapping"], "tool")?;
                let effect = required_str(&config["mapping"], "effect")?;
                let adapter = self
                    .platforms
                    .get(provider)
                    .ok_or_else(|| Error::rejected("bound capability adapter unavailable"))?;
                if !matches!(effect, "read" | "draft")
                    || classify_call(
                        adapter.table(),
                        adapter.reported_manifest_version().as_deref(),
                        tool,
                    ) != if effect == "read" {
                        Effect::Read
                    } else {
                        Effect::Draft
                    }
                {
                    return Err(Error::rejected(
                        "bound app capability is not a current read/draft action",
                    ));
                }
                let input = json!({});
                let input_digest = app_runs::material_digest(&input);
                let request = format!("host-{run_id}-{slot}");
                let call_id = format!(
                    "app-call-{}",
                    uuid::Uuid::new_v5(
                        &uuid::Uuid::NAMESPACE_OID,
                        format!("{run_id}:{slot}").as_bytes(),
                    )
                    .simple()
                );
                let authority = json!({
                    "schema":1,"run_id":run_id,"run_snapshot_digest":run["snapshot_digest"],
                    "install_id":install,"context_id":run["context_id"],
                    "step_id":step_id,"slot":slot,"binding":proof,
                    "inputs":run["snapshot"]["inputs"],"source":run["snapshot"]["source"],
                    "quote":run["snapshot"]["quotes"][slot],
                    "call_id":call_id,
                });
                if self
                    .store
                    .app_capability_result_for_slot(run_id, slot)?
                    .is_some()
                {
                    continue;
                }
                let credential = self.app_capability_credential(config)?;
                let newly_claimed = self.store.app_capability_claim(
                    crate::store::app_capabilities::AppCapabilityClaim {
                        run: run_id,
                        step: &step_id,
                        message: "",
                        turn: "",
                        slot,
                        request: &request,
                        binding_digest: &proof.digest,
                        input_digest: &input_digest,
                        call_id: &call_id,
                    },
                )?;
                if !newly_claimed {
                    let reason = "A prior provider call has no retained receipt; its outcome is uncertain and no automatic retry was made.";
                    self.store
                        .app_run_host_step_failed(run_id, &step_id, "uncertain", reason)?;
                    return Err(Error::unknown(reason));
                }
                let output = match adapter.execute_app_capability_outcome(
                    &credential,
                    &authority,
                    &input,
                    &call_id,
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        let message = if crate::platform::refuse_leak(
                            "app capability failure",
                            error.reason(),
                            &credential,
                        )
                        .is_ok()
                        {
                            error.reason().chars().take(250).collect::<String>()
                        } else {
                            "Provider failure details were withheld because they matched credential custody.".into()
                        };
                        self.store.app_run_host_step_failed(
                            run_id,
                            &step_id,
                            error.kind(),
                            &message,
                        )?;
                        return Err(match error {
                            crate::platform::AppCapabilityError::Refused(_) => {
                                Error::provider(message)
                            }
                            crate::platform::AppCapabilityError::Uncertain(_) => {
                                Error::unknown(message)
                            }
                        });
                    }
                };
                if crate::platform::refuse_leak(
                    "app capability result",
                    &output.result.to_string(),
                    &credential,
                )
                .is_err()
                    || output.asset.as_ref().is_some_and(|asset| {
                        crate::platform::refuse_leak(
                            "app capability asset",
                            &String::from_utf8_lossy(&asset.bytes),
                            &credential,
                        )
                        .is_err()
                    })
                {
                    let reason = "Provider output could not be safely retained; the outcome is uncertain and no automatic retry was made.";
                    self.store
                        .app_run_host_step_failed(run_id, &step_id, "uncertain", reason)?;
                    return Err(Error::unknown(reason));
                }
                if let Err(_error) = self.store.app_capability_record(
                    crate::store::app_capabilities::AppCapabilityRecord {
                        id: &call_id,
                        run: run_id,
                        step: &step_id,
                        message: "",
                        turn: "",
                        slot,
                        request: &request,
                        binding_digest: &proof.digest,
                        input_digest: &input_digest,
                        result: &output.result,
                        asset: output
                            .asset
                            .as_ref()
                            .map(|asset| (asset.media_type.as_str(), asset.bytes.as_slice())),
                    },
                ) {
                    let reason = "Provider returned a result that could not be durably retained; the outcome is uncertain and no automatic retry was made.";
                    self.store
                        .app_run_host_step_failed(run_id, &step_id, "uncertain", reason)?;
                    return Err(Error::unknown(reason));
                }
            }
            self.store.app_run_host_step_succeeded(run_id, &step_id)?;
            Ok(())
        })
    }
    pub(super) fn advance_app_runs(&self) {
        if self.draining() {
            return;
        }
        if let Ok(runs) = self.store.app_run_pending() {
            for (id, _, _) in runs {
                if matches!(self.dispatch_app_run(&id), Err(e) if e.kind() == "rejected") {
                    let _ = self.store.app_run_invalidate(&id);
                }
            }
        }
    }
    fn app_artifact_current(&self, id: &str, turn: Option<(&str, &str)>) -> Result<Value> {
        let install = self.store.app_artifact_installation(id)?;
        let run_id = self.store.app_artifact_run_id(id)?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, &install, |row, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.app_run_binding_current(&self.store.app_run_show(&run_id)?, row, files)?;
            self.store
                .app_artifact_with_digest(id, turn, row["digest"].as_str().unwrap())
        })
    }
}

impl Shared {
    pub(super) fn take_app_aware(&self, alias: &str) -> Result<Take> {
        let head = self.store.queued_head(alias)?;
        if let Some(message) = head.filter(|m| m.source == "app_run_dispatch") {
            let admission = (|| {
                let (run_id, install) = self
                    .store
                    .app_message_installation(&message.id)?
                    .ok_or_else(|| Error::rejected("app message association is absent"))?;
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(&pm, &install, |row, files| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    self.app_run_binding_current(&self.store.app_run_show(&run_id)?, row, files)?;
                    self.store.take_queued_app_proven(
                        alias,
                        Some((&message.id, row["digest"].as_str().unwrap())),
                    )
                })
            })();
            match admission {
                Ok(take) => Ok(take),
                Err(_) => {
                    self.store.reject_app_submission(&message.id)?;
                    Ok(Take::Empty)
                }
            }
        } else {
            self.store.take_queued(alias)
        }
    }
    pub(super) fn admit_app_submission(&self, message: &Message) -> Result<()> {
        if message.source != "app_run_dispatch" {
            return Ok(());
        }
        let (run_id, install) = self
            .store
            .app_message_installation(&message.id)?
            .ok_or_else(|| Error::rejected("app association is absent"))?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, &install, |row, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.app_run_binding_current(&self.store.app_run_show(&run_id)?, row, files)?;
            self.store
                .app_message_admit(message, row["digest"].as_str().unwrap())
        })
    }
}

#[cfg(all(test, unix, feature = "test-seam"))]
mod cad1156_boundary_tests;
#[cfg(all(test, unix, feature = "test-seam"))]
mod cad1156_downstream_tests;
#[cfg(all(test, unix, feature = "test-seam"))]
mod cad1156_escaped_result_tests;
#[cfg(all(test, unix, feature = "test-seam"))]
mod cad1156_summary_tests;
#[cfg(all(test, unix, feature = "test-seam"))]
mod cad1156_tests;

#[cfg(test)]
mod cad1120_tests;
#[cfg(test)]
mod cad1123_acceptance;
#[cfg(test)]
mod cad1123_tests;

#[cfg(test)]
mod cad742_tests {
    use super::*;

    #[test]
    fn frozen_prompt_resolution_records_origin_and_ignores_later_context_edits() {
        let template = crate::issue::workflow::parse_template(include_str!(
            "../../workspace-apps/social-content/workflows/image-manual.md"
        ))
        .unwrap();
        let saved = crate::store::app_contexts::ContextConfig::new(
            "Fav Limited",
            BTreeMap::from([
                ("content_prompt".into(), "Explain a customer benefit".into()),
                ("image_prompt".into(), "Show a quiet desk".into()),
            ]),
        )
        .unwrap();
        let supplied = BTreeMap::from([
            ("content_prompt".into(), "".into()),
            ("image_prompt".into(), "Use a blue background".into()),
            ("source".into(), "Operator supplied product facts".into()),
        ]);
        let (frozen, origins) = freeze_content_inputs(&template, supplied, Some(&saved));
        assert_eq!(frozen["content_prompt"], "Explain a customer benefit");
        assert_eq!(frozen["image_prompt"], "Use a blue background");
        assert_eq!(frozen["source"], "Operator supplied product facts");
        assert_eq!(origins["content_prompt"], "context_default");
        assert_eq!(origins["image_prompt"], "run_override");

        let changed = crate::store::app_contexts::ContextConfig::new(
            "Fav Limited",
            BTreeMap::from([("content_prompt".into(), "A changed default".into())]),
        )
        .unwrap();
        let (next, next_origins) =
            freeze_content_inputs(&template, BTreeMap::new(), Some(&changed));
        assert_eq!(frozen["content_prompt"], "Explain a customer benefit");
        assert_eq!(next["content_prompt"], "A changed default");
        assert_eq!(next_origins["image_prompt"], "app_default");
        assert_eq!(
            next["image_prompt"],
            "Create one editorial image grounded only in the source facts."
        );
    }
}
