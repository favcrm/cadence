//! Operator-owned local app lifecycle and turn-bound dependency artifacts.
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_runs::{self, LocalRunProvenance, LocalRunRequest, LocalWorkflow};
use std::collections::BTreeMap;

/// Release live dispatch ownership on every exit, including provider failure.
/// The mutex protects only membership, never provider I/O.
struct HostRunInFlight<'a> {
    runs: &'a Mutex<HashSet<String>>,
    id: &'a str,
}

impl Drop for HostRunInFlight<'_> {
    fn drop(&mut self) {
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(self.id);
    }
}

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

fn library_reference_inputs(inputs: &BTreeMap<String, String>) -> bool {
    inputs.len() == 4
        && [
            "library_asset_id",
            "library_revision",
            "library_sha256",
            "instructions",
        ]
        .iter()
        .all(|key| inputs.get(*key).is_some_and(|value| !value.is_empty()))
}

pub(super) fn library_reference_inputs_value(inputs: &Value) -> bool {
    let Some(object) = inputs.as_object() else {
        return false;
    };
    object.len() == 4
        && [
            "library_asset_id",
            "library_revision",
            "library_sha256",
            "instructions",
        ]
        .iter()
        .all(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
}

impl Shared {
    /// CAD-1123 HP2: create, approve and dispatch in one operator call. The
    /// operator proof was taken by the caller. The approval is the daemon's
    /// own: it binds exactly the snapshot digest this call just created.
    fn start_app_run(self: &Arc<Self>, params: &Value, human: Option<&Value>) -> Result<Value> {
        let install = required_str(params, "install_id")?;
        // CAD-1171: a host-execution run has no owner, so it needs no
        // default team. `create_app_run` applies the floor only when the
        // installed workflow actually declares `execution: agent`.
        let team = super::app_teams_rpc::team_of(&self.state_dir, install)?;
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
        let created = self.create_app_run_as(params, team.as_ref(), Some(expected), human)?;
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
                    .app_run_decide_as(&id, Some(&digest), false, Some(bundle), human)
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
            run = self.dispatch_started_run_as(&id, replayed, human)?;
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
    #[cfg(test)]
    fn dispatch_started_run(&self, id: &str, replayed: bool) -> Result<Value> {
        self.dispatch_started_run_as(id, replayed, None)
    }

    fn dispatch_started_run_as(
        &self,
        id: &str,
        replayed: bool,
        human: Option<&Value>,
    ) -> Result<Value> {
        match self.dispatch_app_run_as(id, human) {
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

    /// CAD-1365: the verified native creator of this run, frozen into
    /// the immutable snapshot. The trusted host relays the request's
    /// real session credential on `token`/`key`/`origin`; a
    /// credential-less operator connection is honestly
    /// `{kind:"operator"}`. If ANY credential field is present the
    /// complete proof must classify and verify through the existing
    /// `tool_session` seam — a partial, malformed or dead credential
    /// refuses, never an operator fallback. A verified named public
    /// session freezes `{kind:"user", id:<BoardUser.sub>}` and must
    /// carry the operator role, the same public-surface floor
    /// `app_screen_mint` and the media verbs keep. A supplied `key`
    /// must be a string when present; on the public surface the
    /// session check ignores it (`tool_session`'s contract), on
    /// loopback/tailnet a wrong or absent key fails the session proof.
    /// Body-selected creator/user/actor fields never reach here — the
    /// closed field list refuses them.
    fn run_creator(&self, params: &Value) -> Result<Value> {
        let relayed = ["token", "key", "origin"]
            .iter()
            .any(|field| params.get(field).is_some());
        if !relayed {
            return Ok(json!({"kind": "operator"}));
        }
        let token = params
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("run creation needs a session credential token"))?;
        let key = match params.get("key") {
            None => "",
            Some(value) => value
                .as_str()
                .ok_or_else(|| Error::rejected("run session credential key must be a string"))?,
        };
        let origin = params
            .get("origin")
            .and_then(Value::as_str)
            .and_then(crate::operator_auth::Origin::parse)
            .ok_or_else(|| Error::rejected("run creation needs a classified session origin"))?;
        let (_session, view) = self.tool_session(token, key, origin).ok_or_else(|| {
            Error::rejected("run creation needs a live operator session credential")
        })?;
        if origin == crate::operator_auth::Origin::Public {
            let operator = view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator);
            if !operator {
                return Err(Error::rejected(
                    "run creation needs an operator-role public session",
                ));
            }
        }
        Ok(match &view.user {
            Some(user) => json!({"kind": "user", "id": user.sub}),
            None => json!({"kind": "operator"}),
        })
    }

    /// The one run-creation path: `app_run_create` (owner and workers named by
    /// the caller) and `app_run_start` (owner and workers from the install's
    /// default team, quotes checked against `expected`). Operator proof is the
    /// caller's job.
    pub(super) fn create_app_run_as(
        &self,
        params: &Value,
        team: Option<&super::app_teams_rpc::Team>,
        expected: Option<&Value>,
        human: Option<&Value>,
    ) -> Result<Value> {
        // CAD-1171: a host-execution run names no owner PM — the operator's
        // own click executes its capability steps. An agent run still needs
        // one, from the caller or the install's default team.
        // CAD-1365: resolve the native creator BEFORE any workflow,
        // quote or provider work. A credential-less operator
        // connection (the caller gate already ran) is honestly
        // `{kind:"operator"}`; a relayed credential must fully verify
        // through `tool_session` — partial or invalid proofs refuse,
        // never an operator fallback.
        let creator = self.run_creator(params)?;
        let account_actor = self.account_image_actor(params)?;
        // Owner resolution needs the installed template's declared
        // execution, so it happens inside the runtime snapshot below.
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
        let (
            bundle_digest,
            workflow,
            inputs,
            input_origins,
            owner_pm,
            context,
            binding,
            capabilities,
            requests,
        ) = workspace::with_runtime_snapshot(&pm, id, |row, files| {
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
            // CAD-1171/CAD-1365: only an agent workflow needs a team and
            // an owner PM. A host-execution run names no owner — the
            // operator's own click executes the capability — so its
            // effective team is none: `app_run_start` on a host workflow
            // must not require or apply an installation default team,
            // and any supplied `owner_pm` (rejected by the store's
            // host-forbids-owner rule) must not be silently adopted.
            let effective_team = match template.execution {
                crate::issue::workflow::Execution::Host => None,
                crate::issue::workflow::Execution::Agent => {
                    if expected.is_some() && team.is_none() {
                        return Err(Error::rejected(
                            "installation has no default team; set one in Settings",
                        ));
                    }
                    team
                }
            };
            let owner_pm = match effective_team {
                Some(team) => Some(team.owner_pm.clone()),
                None => optional_str(params, "owner_pm").map(str::to_string),
            };
            if let Some(team) = effective_team {
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
            let mut requests = BTreeMap::new();
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
                    .app_binding_auto_image(
                        id,
                        context.as_ref().map(|(_, proof)| proof.id.as_str()),
                        slot,
                        &account_actor,
                        row,
                        files,
                    )?
                    .ok_or_else(|| Error::rejected("run capability binding is absent"))?;
                self.app_account_binding_owned(&account_actor, &proof)?;
                let mut request = self.prepare_app_capability_quote(&proof)?;
                if library_reference_inputs(&inputs)
                    && declared.capability == "media.generate"
                    && declared.effect == "draft"
                {
                    request = request.reference_edit();
                }
                requests.insert(slot.clone(), request);
                capabilities.insert(slot.clone(), proof);
            }
            Ok((
                required_str(row, "digest")?.to_owned(),
                workflow,
                inputs,
                input_origins,
                owner_pm,
                context,
                binding,
                capabilities,
                requests,
            ))
        })?;
        let quotes = requests
            .into_iter()
            .map(|(slot, request)| Ok((slot, request.fetch()?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        if self.run_creator(params)? != creator
            || self.account_image_actor(params)? != account_actor
        {
            return Err(Error::rejected(
                "native run creator changed during price discovery",
            ));
        }
        workspace::with_runtime_snapshot(&pm, id, |row, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.account_image_actor_live(&account_actor)?;
            if row["digest"] != bundle_digest {
                return Err(Error::rejected(
                    "installation changed during price discovery",
                ));
            }
            let context_id = context.as_ref().map(|(_, proof)| proof.id.as_str());
            if let (Some(slot), Some(proof)) =
                (workflow.publication_slot.as_deref(), binding.as_ref())
            {
                self.app_binding_receipt_current(id, context_id, slot, proof, row, files)?;
            }
            for (slot, proof) in &capabilities {
                self.app_binding_receipt_current(id, context_id, slot, proof, row, files)?;
                self.app_account_binding_owned(&account_actor, proof)?;
                let quote = quotes
                    .get(slot)
                    .ok_or_else(|| Error::rejected("run capability quote is absent"))?;
                self.recheck_app_capability_quote(proof, quote)?;
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
            self.store.app_run_create_attributed(
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
                    human,
                },
                Some(&creator),
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
        match self.record_install_consent_checked(pm, install, digest, via, &|| Ok(())) {
            Ok(Ok(_)) => json!({"recorded": true, "via": via, "digest": digest}),
            Ok(Err(error)) | Err(error) => json!({"recorded": false, "via": via, "digest": digest,
                "reason": error.to_string()}),
        }
    }

    /// CAD-1374: the attributed owner's consent. `guard` re-proves the human
    /// immediately before the consent mutation, after the snapshot and contract
    /// I/O. A guard refusal is an `Err` the caller must propagate; it is never
    /// reported as a recorded consent. `changed` is false when the same digest
    /// was already approved, so no authority changed.
    pub(super) fn record_install_consent_guarded(
        &self,
        pm: &crate::issue::Pm,
        install: &str,
        digest: &str,
        via: &str,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<(Value, bool)> {
        Ok(
            match self.record_install_consent_checked(pm, install, digest, via, guard)? {
                Ok(changed) => (
                    json!({"recorded": true, "via": via, "digest": digest}),
                    changed,
                ),
                Err(error) => (
                    json!({"recorded": false, "via": via, "digest": digest,
                        "reason": error.to_string()}),
                    false,
                ),
            },
        )
    }

    /// The consent write. The outer `Err` is a guard refusal, which must not
    /// become success. The inner `Err` is a contract or I/O refusal, which keeps
    /// the existing warning behavior of [`Self::record_install_consent`].
    fn record_install_consent_checked(
        &self,
        pm: &crate::issue::Pm,
        install: &str,
        digest: &str,
        via: &str,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<std::result::Result<bool, Error>> {
        let mut stale = None;
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
            // Fresh authority, read after the I/O above and right before the write.
            if let Err(error) = guard() {
                stale = Some(error);
                return Err(Error::rejected(
                    "the board session changed; the consent was not recorded",
                ));
            }
            self.store
                .app_install_consent(install, digest, via, &capabilities)
        });
        if let Some(error) = stale {
            return Err(error);
        }
        Ok(outcome.map(|changed| changed.is_some()))
    }

    pub(super) fn rpc_app_local(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let allowed: &[&str] = match method {
            "app_local_install_revoke" => &[
                "install_id",
                "digest",
                "public_session",
                "expected_generation",
                "hosted_operation_id",
            ],
            "app_local_install_approve" => &["install_id", "digest", "public_session"],
            // CAD-1365: create/start additionally admit the trusted
            // host's relayed session credential (token/key/origin) —
            // resolved natively before any run work, never a
            // body-selected identity. Other lifecycle verbs keep their
            // exact field sets.
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
                "public_session",
                "token",
                "key",
                "origin",
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
                "public_session",
                "token",
                "key",
                "origin",
            ],
            "app_run_approve" => &["run_id", "digest", "public_session"],
            "app_run_cancel" | "app_run_dispatch" => &["run_id", "public_session"],
            "app_run_show" => &["run_id"],
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
        // CAD-1374: a board owner's write is attributed to its verified human.
        // Only the owner lifecycle verbs may carry a public proof; an agent
        // grant holder never does (a present proof is a refused attempt).
        let human = match (method, params.get("public_session")) {
            (_, None) => None,
            (
                "app_local_install_approve"
                | "app_local_install_revoke"
                | "app_run_approve"
                | "app_run_cancel"
                | "app_run_dispatch"
                | "app_run_start"
                | "app_run_create",
                Some(_),
            ) => {
                // An agent (registered pane or managed endpoint) never carries
                // a human proof, even through the grant exception.
                if self.slot_identity(peer_pid)?.is_some() {
                    return Err(Error::rejected(
                        "agent connections cannot carry a board owner's proof",
                    ));
                }
                // The canonical verified human: stable identity, company, session
                // reference, the effective role and the display label. Only
                // the verified human is stamped, never the request's own fields.
                self.verified_owner_human(params)?.map(|human| {
                    json!({
                        "sub": human["sub"],
                        "company": human["company"],
                        "session": human["session"],
                        "role": human["role"],
                        "display": human["display"],
                    })
                })
            }
            _ => {
                return Err(Error::rejected(
                    "app lifecycle payload has unsupported fields",
                ))
            }
        };
        let human = human.as_ref();
        match method {
            "app_local_install_approve" | "app_local_install_revoke" => {
                let pm = self.pm_at(&self.pm_dir()?)?;
                let generation = params
                    .get("expected_generation")
                    .map(|_| required_str(params, "expected_generation"))
                    .transpose()?;
                let hosted_operation_id = params
                    .get("hosted_operation_id")
                    .map(|_| required_str(params, "hosted_operation_id"))
                    .transpose()?;
                if let Some(id) = hosted_operation_id {
                    crate::proto::identifier(id, "hosted operation ID")?;
                    if generation.is_none() {
                        return Err(Error::rejected(
                            "hosted revoke requires a catalog generation",
                        ));
                    }
                }
                let mutation_started = std::cell::Cell::new(false);
                let result = workspace::with_runtime_snapshot(
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
                        // The snapshot and the SQLite decision share this held PM
                        // lock, so another catalog writer cannot slip past the pin.
                        if generation.is_some_and(|expected| {
                            row["catalog_generation"].as_str() != Some(expected)
                        }) {
                            return Err(Error::rejected("workspace catalog generation is stale"));
                        }
                        if method == "app_local_install_approve" {
                            local_execution_contract(files)?;
                        }
                        mutation_started.set(true);
                        self.store.app_capability_decide_as(
                            required_str(params, "install_id")?,
                            digest,
                            method == "app_local_install_approve",
                            human,
                        )
                    },
                );
                workspace::hosted_mutation_outcome(
                    result,
                    mutation_started.get(),
                    hosted_operation_id,
                    "revoke",
                )
            }
            "app_run_create" => self.create_app_run_as(params, None, None, human),
            "app_run_start" => self.start_app_run(params, human),
            "app_run_approve" => {
                let id = required_str(params, "run_id")?;
                self.with_app_run_current(id, |current_bundle| {
                    self.store.app_run_decide_as(
                        id,
                        Some(required_str(params, "digest")?),
                        false,
                        Some(current_bundle),
                        human,
                    )
                })
            }
            "app_run_cancel" => {
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.store.app_run_decide_as(
                    required_str(params, "run_id")?,
                    None,
                    true,
                    None,
                    human,
                )
            }
            "app_run_dispatch" => {
                let run = self.dispatch_app_run_as(required_str(params, "run_id")?, human)?;
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
    /// Capture only local receipts under the established lock order. The
    /// caller performs price I/O unlocked, then rechecks these same receipts.
    pub(super) fn prepare_app_run_quotes(
        &self,
        run: &Value,
    ) -> Result<BTreeMap<String, Result<super::app_capabilities_rpc::CapabilityQuoteRequest>>> {
        let install = required_str(run, "install_id")?;
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
            let mut requests = BTreeMap::new();
            let reference_edit = library_reference_inputs_value(&run["snapshot"]["inputs"]);
            for (slot, binding) in run["snapshot"]["capabilities"]
                .as_object()
                .into_iter()
                .flatten()
            {
                let proof = serde_json::from_value(binding.clone())
                    .map_err(|_| Error::rejected("run capability binding receipt is invalid"))?;
                let request = self.prepare_app_capability_quote(&proof).map(|request| {
                    if reference_edit
                        && proof.config["declaration"]["capability"] == "media.generate"
                        && proof.config["declaration"]["effect"] == "draft"
                    {
                        request.reference_edit()
                    } else {
                        request
                    }
                });
                requests.insert(slot.clone(), request);
            }
            Ok(requests)
        })
    }

    pub(super) fn probe_app_run_prices(&self, run: &Value) -> Result<()> {
        // As before, a failed PRICE probe is not proof of authority loss.
        // The final locked receipt check remains authoritative.
        if let Ok(requests) = self.prepare_app_run_quotes(run) {
            for (slot, request) in requests {
                if let Ok(quote) = request.and_then(|request| request.fetch()) {
                    if run["snapshot"]["quotes"][&slot] != json!(quote) {
                        return Err(Error::rejected(
                            "capability price changed since run creation",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn with_app_run_current<T>(
        &self,
        id: &str,
        callback: impl FnOnce(&str) -> Result<T>,
    ) -> Result<T> {
        let run = self.store.app_run_show(id)?;
        self.probe_app_run_prices(&run)?;
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
                // Price liveness is probed by the caller before taking these
                // locks. This is the final, local authority recheck.
            }
        }
        Ok(())
    }
    pub(super) fn dispatch_app_run(&self, id: &str) -> Result<Value> {
        self.dispatch_app_run_as(id, None)
    }
    /// CAD-1374: dispatch under the approving human's fence. The human's
    /// freshness is re-proven BEFORE any new privileged step is enqueued, and
    /// the transaction re-checks that the approval is still the one proven.
    /// An owner-less (legacy/local) approval skips the fence; a human-attributed
    /// run whose approver is no longer a live owner refuses here.
    pub(super) fn dispatch_app_run_as(&self, id: &str, human: Option<&Value>) -> Result<Value> {
        let run = self.store.app_run_show(id)?;
        // Check and take ownership atomically BEFORE dispatch can publish a
        // running host step. A monitor/replay observing that commit must leave
        // the live executor alone, not mistake its claim for a crashed call.
        let _host_run = if run["snapshot"]["workflow"]["execution"] == "host" {
            let mut runs = self
                .host_runs_in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !runs.insert(id.to_string()) {
                return self.store.app_run_show(id);
            }
            Some(HostRunInFlight {
                runs: &self.host_runs_in_flight,
                id,
            })
        } else {
            None
        };
        let approved_by = run["approval"]["human"]
            .as_object()
            .map(|_| run["approval"]["human"].clone());
        if let Some(sealed) = &approved_by {
            self.fresh_run_human(sealed)?;
        }
        let account_run = run["snapshot"]["capabilities"]
            .as_object()
            .is_some_and(|slots| {
                slots.values().any(|proof| {
                    proof["config"]["provider"] == crate::platform::account_media::PLATFORM
                })
            });
        let result = if account_run {
            self.account_frozen_lineage(&run)?;
            self.probe_app_run_prices(&run)?;
            if let Some(sealed) = &approved_by {
                self.fresh_run_human(sealed)?;
            }
            let pm = self.pm_at(&self.pm_dir()?)?;
            workspace::with_runtime_snapshot(
                &pm,
                required_str(&run, "install_id")?,
                |bundle, files| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let current = self.store.app_run_show(id)?;
                    if current["snapshot"] != run["snapshot"]
                        || current["approval"]["human"] != run["approval"]["human"]
                    {
                        return Err(Error::rejected(
                            "account run authority changed before dispatch",
                        ));
                    }
                    self.app_run_binding_current(&current, bundle, files)?;
                    self.store.app_run_dispatch_as(
                        id,
                        required_str(bundle, "digest")?,
                        approved_by.as_ref(),
                        human,
                    )
                },
            )?
        } else {
            self.with_app_run_current(id, |digest| {
                if let Some(sealed) = &approved_by {
                    self.fresh_run_human(sealed)?;
                }
                self.store
                    .app_run_dispatch_as(id, digest, approved_by.as_ref(), human)
            })?
        };
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

    /// Re-pin immutable image material before taking runtime snapshot locks.
    /// The consumer must prove the producing authority under its own guards.
    pub(super) fn library_reference_pin(
        &self,
        inputs: &Value,
    ) -> Result<crate::store::app_media::MediaOriginalProof> {
        let asset_id = inputs["library_asset_id"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::rejected("library image id is not pinned"))?;
        let revision = inputs["library_revision"]
            .as_str()
            .and_then(|raw| raw.parse::<i64>().ok())
            .filter(|revision| *revision >= 1)
            .ok_or_else(|| Error::rejected("library image revision is not pinned"))?;
        let sha256 = inputs["library_sha256"]
            .as_str()
            .filter(|value| {
                value.len() == 71
                    && value.starts_with("sha256:")
                    && value[7..]
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
            .ok_or_else(|| Error::rejected("library image digest is not pinned"))?;
        if inputs["instructions"]
            .as_str()
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(Error::rejected("reference instructions are blank"));
        }
        let original = self.store.app_media_pin(asset_id, revision, sha256)?;
        let hex = &sha256[7..];
        let reference_ok = original
            .reference
            .split_once('.')
            .is_some_and(|(job, digest)| {
                digest == hex && job.starts_with("med_") && !original.reference.contains("://")
            });
        if original.sha256 != sha256 || !reference_ok {
            return Err(Error::rejected(
                "producer reference is not the retained original artifact",
            ));
        }
        Ok(original)
    }

    /// Producer plus original pin, for a caller already inside the paired
    /// runtime snapshot with custody then release held. No remote I/O.
    pub(super) fn library_reference_under(
        &self,
        inputs: &Value,
        expected: &crate::store::app_media::MediaOriginalProof,
        producer_bundle: &Value,
        producer_files: &BTreeMap<String, String>,
    ) -> Result<Value> {
        self.app_binding_receipt_library_source(
            &expected.install_id,
            expected.context_id.as_deref(),
            &expected.slot,
            &expected.proof,
            producer_bundle,
            producer_files,
        )?;
        if self.store.app_capability_status(
            &expected.install_id,
            required_str(producer_bundle, "digest")?,
        )?["state"]
            != "approved"
        {
            return Err(Error::rejected(
                "reference producing installation is not approved",
            ));
        }
        let pinned = self.library_reference_pin(inputs)?;
        if pinned.sha256 != expected.sha256
            || pinned.reference != expected.reference
            || pinned.install_id != expected.install_id
            || pinned.slot != expected.slot
        {
            return Err(Error::rejected(
                "producer reference changed before the reference edit",
            ));
        }
        Ok(json!({
            "asset_id": inputs["library_asset_id"],
            "sha256": pinned.sha256,
            "ref": pinned.reference,
        }))
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
        let quotes = self
            .prepare_app_run_quotes(run)?
            .into_iter()
            .map(|(slot, request)| Ok((slot, request?.fetch()?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let account_slots: Vec<_> = slots
            .iter()
            .filter(|slot| {
                run["snapshot"]["capabilities"][*slot]["config"]["provider"]
                    == crate::platform::account_media::PLATFORM
            })
            .collect();
        let reference_original = if account_slots.len() != slots.len()
            && library_reference_inputs_value(&run["snapshot"]["inputs"])
        {
            Some(self.library_reference_pin(&run["snapshot"]["inputs"])?)
        } else {
            None
        };
        let pm = self.pm_at(&self.pm_dir()?)?;
        // CAD-1258: per legacy slot, lock 1 verifies and claims, the provider
        // call runs with NO PM lock, and lock 2 re-verifies the same authority
        // before the result is retained. Mixed runs release only at the
        // necessary account boundaries.
        let execute_legacy = |legacy_slots: &[String], finish: bool| -> Result<()> {
            type Files = std::collections::BTreeMap<String, String>;
            type Held<'a> = Option<(&'a Value, &'a Files)>;
            let snap = |f: &mut dyn FnMut(&Value, &Files, Held) -> Result<()>| -> Result<()> {
                if let Some(original) = &reference_original {
                    workspace::with_runtime_snapshot_pair(
                        &pm,
                        install,
                        &original.install_id,
                        |(bundle, files), producer| f(bundle, files, Some(producer)),
                    )
                } else {
                    workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
                        f(bundle, files, None)
                    })
                }
            };
            // Called with the custody and release guards held.
            let current =
                |bundle: &Value, files: &Files, producer: Option<(&Value, &Files)>| -> Result<()> {
                    if let Some(original) = &reference_original {
                        let (producer_bundle, producer_files) = producer.ok_or_else(|| {
                            Error::rejected("reference producer snapshot is unavailable")
                        })?;
                        self.app_binding_receipt_library_source(
                            &original.install_id,
                            original.context_id.as_deref(),
                            &original.slot,
                            &original.proof,
                            producer_bundle,
                            producer_files,
                        )?;
                        if self.store.app_capability_status(
                            &original.install_id,
                            required_str(producer_bundle, "digest")?,
                        )?["state"]
                            != "approved"
                        {
                            return Err(Error::rejected(
                                "reference producing installation is not approved",
                            ));
                        }
                    }
                    self.app_run_binding_current(run, bundle, files)
                };
            for slot in legacy_slots {
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
                let mut prepared = None;
                snap(&mut |bundle, files, producer| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    current(bundle, files, producer)?;
                    let proof = self
                        .app_binding_live(install, run["context_id"].as_str(), slot, bundle, files)?
                        .ok_or_else(|| Error::rejected("run capability binding is absent"))?;
                    let quote = quotes
                        .get(slot)
                        .ok_or_else(|| Error::rejected("run capability quote is absent"))?;
                    if run["snapshot"]["quotes"][slot] != json!(quote) {
                        return Err(Error::rejected(
                            "capability price changed since run approval",
                        ));
                    }
                    self.recheck_app_capability_quote(&proof, quote)?;
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
                    let mut authority = json!({
                        "schema":1,"run_id":run_id,"run_snapshot_digest":run["snapshot_digest"],
                        "install_id":install,"context_id":run["context_id"],
                        "step_id":step_id,"slot":slot,"binding":proof,
                        "inputs":run["snapshot"]["inputs"],"source":run["snapshot"]["source"],
                        "quote":run["snapshot"]["quotes"][slot],
                        "call_id":call_id,
                    });
                    if effect == "draft"
                        && config["mapping"]["capability"] == "media.generate"
                        && library_reference_inputs_value(&run["snapshot"]["inputs"])
                    {
                        let asset_id = run["snapshot"]["inputs"]["library_asset_id"]
                            .as_str()
                            .unwrap_or("");
                        let revision = run["snapshot"]["inputs"]["library_revision"]
                            .as_str()
                            .and_then(|raw| raw.parse::<i64>().ok())
                            .filter(|n| *n >= 1)
                            .ok_or_else(|| {
                                Error::rejected("library image revision is not pinned")
                            })?;
                        let sha256 = run["snapshot"]["inputs"]["library_sha256"]
                            .as_str()
                            .ok_or_else(|| Error::rejected("library image digest is not pinned"))?;
                        // The paired snapshot and these held custody/release guards
                        // prove the producer as well as the consuming run.
                        let original = self.store.app_media_pin(asset_id, revision, sha256)?;
                        if original.sha256 != sha256
                            || reference_original
                                .as_ref()
                                .is_none_or(|expected| original.reference != expected.reference)
                        {
                            return Err(Error::rejected(
                                "producer reference changed before the reference edit",
                            ));
                        }
                        authority["reference_edit"] = json!({
                            "asset_id": asset_id,
                            "sha256": sha256,
                            "ref": original.reference,
                        });
                        authority["frozen_prompt"] =
                            run["snapshot"]["inputs"]["instructions"].clone();
                    }
                    if self
                        .store
                        .app_capability_result_for_slot(run_id, slot)?
                        .is_some()
                    {
                        return Ok(());
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
                        self.store.app_run_host_step_failed(
                            run_id,
                            &step_id,
                            "uncertain",
                            reason,
                        )?;
                        return Err(Error::unknown(reason));
                    }
                    // CAD-1374: re-prove the sealed approver after the claim shows
                    // a NEW call and before any provider bytes leave. A revoked or
                    // demoted approver refuses this step; the claim stays for an
                    // honest, non-retried outcome.
                    if let Some(sealed) = run["approval"]["human"]
                        .as_object()
                        .map(|_| &run["approval"]["human"])
                    {
                        self.fresh_run_human(sealed)?;
                    }
                    prepared = Some((std::sync::Arc::clone(adapter), credential, authority, proof));
                    Ok(())
                })?;
                let Some((adapter, credential, authority, proof)) = prepared else {
                    continue;
                };
                crate::issue::lockseam::outside_lock("app-host-provider-call");
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
                // CAD-1258 interleaving point: a revoke or upgrade may land here.
                crate::issue::lockseam::outside_lock("app-host-before-record");
                let uncertain = |reason: &str| -> Error {
                    let _ =
                        self.store
                            .app_run_host_step_failed(run_id, &step_id, "uncertain", reason);
                    Error::unknown(reason)
                };
                let entered = std::cell::Cell::new(false);
                let recorded = snap(&mut |bundle, files, producer| {
                    entered.set(true);
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let live = self.store.app_run_show(run_id)?;
                    let unchanged = current(bundle, files, producer).is_ok()
                        && self
                            .app_binding_live(
                                install,
                                run["context_id"].as_str(),
                                slot,
                                bundle,
                                files,
                            )
                            .ok()
                            .flatten()
                            .is_some_and(|now| now.digest == proof.digest)
                        && live["snapshot_digest"] == run["snapshot_digest"]
                        && live["approval"]["human"] == run["approval"]["human"]
                        && self
                            .store
                            .app_capability_status(install, required_str(bundle, "digest")?)
                            .is_ok_and(|status| {
                                status["state"] == "approved" && status["epoch"] == live["epoch"]
                            });
                    if !unchanged {
                        return Err(uncertain(
                            "App authority changed while the provider call ran; the result was not retained and no automatic retry was made.",
                        ));
                    }
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
                        return Err(uncertain(
                            "Provider output could not be safely retained; the outcome is uncertain and no automatic retry was made.",
                        ));
                    }
                    if self
                        .store
                        .app_capability_record(
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
                                asset: output.asset.as_ref().map(|asset| {
                                    (asset.media_type.as_str(), asset.bytes.as_slice())
                                }),
                            },
                        )
                        .is_err()
                    {
                        return Err(uncertain(
                            "Provider returned a result that could not be durably retained; the outcome is uncertain and no automatic retry was made.",
                        ));
                    }
                    Ok(())
                });
                match recorded {
                    Err(_) if !entered.get() => {
                        // The snapshot itself refused (removed or changed install).
                        return Err(uncertain(
                            "App authority changed while the provider call ran; the result was not retained and no automatic retry was made.",
                        ));
                    }
                    other => other?,
                }
            }
            if finish {
                snap(&mut |bundle, files, producer| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    current(bundle, files, producer)?;
                    self.store.app_run_host_step_succeeded(run_id, &step_id)?;
                    Ok(())
                })?;
            }
            Ok(())
        };
        if account_slots.is_empty() {
            return execute_legacy(&slots, true);
        }
        for slot in &slots {
            if !account_slots.contains(&slot) {
                execute_legacy(std::slice::from_ref(slot), false)?;
                continue;
            }
            let quote = quotes
                .get(slot)
                .ok_or_else(|| Error::rejected("run capability quote is absent"))?;
            if let Err(error) = self.execute_account_run_capability(
                run,
                slot,
                quote,
                (&json!({}), &format!("host-{run_id}-{slot}")),
                None,
            ) {
                let kind = if error.kind() == "unknown" {
                    "uncertain"
                } else {
                    "refused"
                };
                // Revocation may already have ended the run. Preserve the
                // actual call outcome even if marking the step now refuses.
                let _ =
                    self.store
                        .app_run_host_step_failed(run_id, &step_id, kind, &error.to_string());
                return Err(error);
            }
        }
        if run["approval"]["human"].is_object() {
            self.fresh_run_human(&run["approval"]["human"])?;
        }
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
            let current = self.store.app_run_show(run_id)?;
            let status = self
                .store
                .app_capability_status(install, required_str(bundle, "digest")?)?;
            if current["snapshot"] != run["snapshot"]
                || current["snapshot_digest"] != run["snapshot_digest"]
                || current["approval"]["human"] != run["approval"]["human"]
                || status["state"] != "approved"
                || status["epoch"] != current["epoch"]
            {
                return Err(Error::rejected(
                    "account host authority changed before step completion",
                ));
            }
            for slot in &account_slots {
                let proof = serde_json::from_value(run["snapshot"]["capabilities"][*slot].clone())
                    .map_err(|_| Error::rejected("account host binding is absent"))?;
                self.recheck_app_capability_quote(&proof, &quotes[*slot])?;
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
                // CAD-1142: only a rejection — proved authority/assignment
                // loss — invalidates, and the rejection text becomes the
                // steps' persisted reason. A transient probe failure never
                // reaches here as `rejected` (see `app_run_binding_current`).
                if let Err(e) = self.dispatch_app_run(&id) {
                    if e.kind() == "rejected" {
                        let _ = self.store.app_run_invalidate(&id, &e.to_string());
                    }
                }
            }
            #[cfg(feature = "test-seam")]
            super::test_events::completed(
                &self.state_dir,
                super::test_events::Event::AppRunsAdvanced,
            );
        }
    }
    fn app_artifact_current(&self, id: &str, turn: Option<(&str, &str)>) -> Result<Value> {
        let install = self.store.app_artifact_installation(id)?;
        let run_id = self.store.app_artifact_run_id(id)?;
        self.probe_app_run_prices(&self.store.app_run_show(&run_id)?)?;
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
        // CAD-1380 R1: a queued member turn re-proves its ORIGINAL credential
        // before the generic take. A refused row is failed and retained, never
        // dropped, with a bounded event naming the refusal.
        if let Some(message) = head.as_ref().filter(|m| m.source != "app_run_dispatch") {
            if let Err(error) = self.member_message_reprove(message) {
                let reason = error.to_string();
                let refused = json!({"status": "failed", "text": "", "error": reason});
                self.store
                    .finish(message, "failed", &refused, Some(&reason))?;
                self.store.event_public(
                    alias,
                    "member_turn_refused",
                    json!({"message": message.id, "reason": reason}),
                )?;
                return Ok(Take::Empty);
            }
        }
        if let Some(message) = head.filter(|m| m.source == "app_run_dispatch") {
            // CAD-1374: a queued app step is a new execution, so its sealed
            // approver is re-proven first, with no store or auth lock held. A
            // refusal or outage only defers the take: nothing is rejected or
            // cancelled, and no external success is reported.
            let Some((run_id, _)) = self.store.app_message_installation(&message.id)? else {
                self.store.reject_app_submission(&message.id)?;
                return Ok(Take::Empty);
            };
            let checked = self.store.app_run_show(&run_id)?;
            if let Some(sealed) = checked["approval"]["human"]
                .as_object()
                .map(|_| &checked["approval"]["human"])
            {
                if self.fresh_run_human(sealed).is_err() {
                    return Ok(Take::Empty);
                }
            }
            let admission = (|| {
                let (run_id, install) = self
                    .store
                    .app_message_installation(&message.id)?
                    .ok_or_else(|| Error::rejected("app message association is absent"))?;
                self.probe_app_run_prices(&checked)?;
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
                    let run = self.store.app_run_show(&run_id)?;
                    self.app_run_binding_current(&run, row, files)?;
                    // CAD-1374: the approval must still be the one freshly checked
                    // above. A changed binding refuses without laundering it.
                    if run["approval"]["human"] != checked["approval"]["human"] {
                        return Err(Error::gate_coded("approval_changed", "the run approval changed since its owner was checked; the queued step is refused"));
                    }
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
            // CAD-1380 R1: a member turn re-proves at the admission gate, so a
            // revoke after claim cannot reach the adapter. A refusal is a
            // pre-write rejection: the row fails and the endpoint stays live.
            return self
                .member_message_reprove(message)
                .map_err(|e| Error::PreWrite(e.to_string()));
        }
        let (run_id, install) = self
            .store
            .app_message_installation(&message.id)?
            .ok_or_else(|| Error::rejected("app association is absent"))?;
        self.probe_app_run_prices(&self.store.app_run_show(&run_id)?)?;
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
mod cad1230_acceptance;
#[cfg(all(test, feature = "test-seam"))]
mod cad792_acceptance;

#[cfg(test)]
mod cad742_tests {
    use super::*;

    #[test]
    fn frozen_prompt_resolution_records_origin_and_ignores_later_context_edits() {
        let template = crate::issue::workflow::parse_template(include_str!(
            "../../tests/fixtures/apps/social-content/workflows/image-manual.md"
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
