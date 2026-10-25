//! Run-bound read/draft broker. A connection-derived agent proves its exact
//! active app turn; installation, context, action and account are frozen.
use super::app_bindings_rpc::strict_fields;
use super::*;
use crate::contract_fixture::{classify_call, Effect};
use crate::issue::app_catalog::workspace;
use crate::store::app_bindings::BindingProof;
use crate::store::app_runs;
use std::collections::BTreeMap;

const PRICE_REFUSED: &str = "bound capability price discovery refused";

/// Immutable local custody snapshot. `fetch` is called only after dropping
/// the PM, custody and release guards; it owns no daemon/store lock.
pub(super) struct CapabilityQuoteRequest {
    adapter: Arc<dyn crate::platform::PlatformAdapter>,
    credential: Vec<u8>,
    binding: Value,
    reference_edit: bool,
}

impl CapabilityQuoteRequest {
    pub(super) fn reference_edit(mut self) -> Self {
        self.reference_edit = true;
        self
    }

    pub(super) fn fetch(self) -> Result<crate::platform::AppCapabilityQuote> {
        let quote = if self.reference_edit {
            self.adapter
                .quote_reference_app_capability(&self.credential, &self.binding)
        } else {
            self.adapter
                .quote_app_capability(&self.credential, &self.binding)
        }
        .map_err(|error| Error::rejected(price_refusal(&error)))?;
        if !quote.valid() {
            return Err(Error::rejected("bound capability price quote is invalid"));
        }
        Ok(quote)
    }
}

/// CAD-1096: the operator sees why the door refused a quote. Only an
/// `[A-Za-z0-9_]{1,64}` code after the adapter's "refused: " crosses here.
fn price_refusal(error: &str) -> String {
    match error
        .rsplit_once("refused: ")
        .filter(|(_, code)| refusal_code(code))
    {
        Some((_, code)) => format!("{PRICE_REFUSED}: {code}"),
        None => PRICE_REFUSED.into(),
    }
}

fn refusal_code(code: &str) -> bool {
    (1..=64).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The board relays a quote refusal only in the exact [`price_refusal`] shape.
pub(crate) fn operator_price_refusal(message: &str) -> Option<&str> {
    let code = message.strip_prefix(PRICE_REFUSED)?;
    (code.is_empty() || code.strip_prefix(": ").is_some_and(refusal_code)).then_some(message)
}

#[cfg(test)]
mod tests;

impl Shared {
    /// Called only under the broker's custody lock, never by the legacy
    /// credential/grant path. Empty bytes require a live registered builtin
    /// account and its exact frozen connection/descriptor receipts.
    /// `pub(super)` for the CAD-1177 standalone tool entry, which reuses
    /// the identical custody proof.
    pub(super) fn app_capability_credential(&self, config: &Value) -> Result<Vec<u8>> {
        let provider = required_str(config, "provider")?;
        let account = required_str(config, "account")?;
        let adapter = self
            .platforms
            .get(provider)
            .ok_or_else(|| Error::rejected("bound capability adapter unavailable"))?;
        if !adapter.app_credentialless_account(account) {
            if config["connection_kind"] == "builtin"
                && !crate::platform::is_builtin(provider, account)
            {
                return Err(Error::rejected(
                    "bound builtin is not credentialless in this registration",
                ));
            }
            return crate::platform::load_credential(
                &self.store,
                &self.platform_custody,
                provider,
                account,
            );
        }
        let descriptor = self.connection_descriptor(provider)?;
        if config["schema"] != 1
            || config["workspace_id"] != self.store.connection_workspace_id()?
            || config["connection_kind"] != "builtin"
            || !descriptor
                .builtin_accounts
                .iter()
                .any(|builtin| builtin == account)
            || config["descriptor_revision"] != descriptor.revision
            || adapter.connection_registration().as_deref() != config["sink_registration"].as_str()
            || !config["sink_registration"].is_string()
        {
            return Err(Error::rejected(
                "credentialless app connection is not a current builtin",
            ));
        }
        let current = self
            .connection_list_locked()?
            .into_iter()
            .find(|row| row["id"] == config["connection_id"])
            .ok_or_else(|| Error::rejected("credentialless app connection is missing"))?;
        if current["kind"] != "builtin"
            || current["provider"] != provider
            || current["account"] != account
            || current["revision"] != config["connection_revision"]
            || current["registration_digest"] != config["registration_digest"]
            || !current["registration_digest"].is_string()
            || current["status"]["manifest_status"] != "matched"
            || current["status"]["reviewed_pin"] != config["reviewed_pin"]
            || current["status"]["reported_pin"] != config["reported_pin"]
        {
            return Err(Error::rejected(
                "credentialless app connection receipt is stale",
            ));
        }
        Ok(Vec::new())
    }

    pub(super) fn prepare_app_capability_quote(
        &self,
        proof: &BindingProof,
    ) -> Result<CapabilityQuoteRequest> {
        let config = &proof.config;
        // Called under the broker's custody/release section. Refuse before
        // quoting, input digests, claims and cached-success return paths.
        self.refuse_encrypted_binding(config)?;
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
        let credential = self.app_capability_credential(config)?;
        Ok(CapabilityQuoteRequest {
            adapter: Arc::clone(adapter),
            credential,
            binding: serde_json::to_value(proof)?,
            reference_edit: false,
        })
    }

    pub(super) fn recheck_app_capability_quote(
        &self,
        proof: &BindingProof,
        quote: &crate::platform::AppCapabilityQuote,
    ) -> Result<()> {
        self.app_capability_credential(&proof.config)?;
        crate::platform::account_media::recheck_quote(&proof.config, quote)
    }

    #[cfg(test)]
    pub(super) fn app_capability_quote(
        &self,
        proof: &BindingProof,
    ) -> Result<crate::platform::AppCapabilityQuote> {
        self.prepare_app_capability_quote(proof)?.fetch()
    }

    /// Offline refusal for a frozen account quote. Custody is resolved
    /// locally; renewal, price discovery and provider HTTP stay with the caller.
    pub(super) fn account_frozen_lineage(&self, run: &Value) -> Result<()> {
        let Some(slots) = run["snapshot"]["capabilities"].as_object() else {
            return Ok(());
        };
        for (slot, proof) in slots {
            if proof["config"]["provider"] != crate::platform::account_media::PLATFORM {
                continue;
            }
            let revision = run["snapshot"]["quotes"][slot]["price_revision"]
                .as_str()
                .ok_or_else(|| {
                    Error::rejected(
                        "saved account SESSION QUOTE is stale; request a fresh quote and approve it again",
                    )
                })?;
            let identity = crate::platform::account_media::current_identity(&proof["config"])
                .map_err(Error::rejected)?;
            crate::platform::account_media::validate_quote_source(&identity, revision)
                .map_err(Error::rejected)?;
        }
        Ok(())
    }

    /// Account-only run execution. Preparation and retention each take a FRESH
    /// PM/custody/release snapshot; neither provider nor human-authority I/O is
    /// inside either callback. A durable claim admits exactly one sender.
    pub(super) fn execute_account_run_capability(
        &self,
        expected: &Value,
        slot: &str,
        quote: &crate::platform::AppCapabilityQuote,
        invocation: (&Value, &str),
        worker: Option<(&str, &str, &str)>,
    ) -> Result<Value> {
        let (input, request) = invocation;
        let run_id = required_str(expected, "id")?;
        let install = required_str(expected, "install_id")?;
        let (message, turn) = worker
            .map(|(_, message, turn)| (message, turn))
            .unwrap_or(("", ""));
        let input_digest = app_runs::material_digest(input);
        let call_id = format!(
            "app-call-{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("{run_id}:{slot}").as_bytes(),
            )
            .simple()
        );
        let pm = self.pm_at(&self.pm_dir()?)?;
        let current = |bundle: &Value,
                       files: &BTreeMap<String, String>|
         -> Result<(String, BindingProof)> {
            let run = self.store.app_run_show(run_id)?;
            if run["state"] != "running"
                || run["approved_digest"] != run["snapshot_digest"]
                || run["snapshot_digest"] != expected["snapshot_digest"]
                || app_runs::material_digest(&run["snapshot"]) != run["snapshot_digest"]
                || run["snapshot"] != expected["snapshot"]
                || run["approval"]["human"] != expected["approval"]["human"]
                || run["context_id"] != expected["context_id"]
                || run["install_id"] != install
                || run["snapshot"]["bundle_digest"] != bundle["digest"]
                || run["snapshot"]["quotes"][slot] != json!(quote)
            {
                return Err(Error::rejected(
                    "account capability run approval or snapshot changed",
                ));
            }
            let status = self
                .store
                .app_capability_status(install, required_str(bundle, "digest")?)?;
            if status["state"] != "approved" || status["epoch"] != run["epoch"] {
                return Err(Error::rejected("account capability approval epoch changed"));
            }
            self.app_run_binding_current(&run, bundle, files)?;
            let (step, proof) = if let Some((alias, message, turn)) = worker {
                let (active, step, proof) = self.store.app_capability_turn(
                    alias,
                    message,
                    turn,
                    slot,
                    required_str(bundle, "digest")?,
                )?;
                if active["id"] != run_id
                    || active["snapshot_digest"] != run["snapshot_digest"]
                    || active["approval"]["human"] != run["approval"]["human"]
                {
                    return Err(Error::rejected("account capability assigned turn changed"));
                }
                (step, proof)
            } else {
                let step = required_str(&run["snapshot"]["workflow"]["steps"][0], "id")?;
                if run["snapshot"]["workflow"]["execution"] != "host"
                    || !run["steps"].as_array().is_some_and(|steps| {
                        steps
                            .iter()
                            .any(|row| row["step_id"] == step && row["state"] == "dispatched")
                    })
                {
                    return Err(Error::rejected(
                        "account capability host step is not dispatched",
                    ));
                }
                let proof = serde_json::from_value(run["snapshot"]["capabilities"][slot].clone())
                    .map_err(|_| Error::rejected("account capability binding is absent"))?;
                (step.to_string(), proof)
            };
            if proof.config["provider"] != crate::platform::account_media::PLATFORM {
                return Err(Error::rejected("account capability source changed"));
            }
            self.recheck_app_capability_quote(&proof, quote)?;
            Ok((step, proof))
        };
        let prepared = workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let (step, proof) = current(bundle, files)?;
            if let Some(existing) = self.store.app_capability_result_for_slot(run_id, slot)? {
                if existing["step_id"] != step
                    || existing["request_id"] != request
                    || existing["input_digest"] != input_digest
                    || existing["binding_digest"] != proof.digest
                {
                    return Err(Error::rejected(
                        "approved run capability slot already has its one result",
                    ));
                }
                return Ok(std::ops::ControlFlow::Break(existing));
            }
            let prepared = self.prepare_app_capability_quote(&proof)?;
            if !self.store.app_capability_claim(
                crate::store::app_capabilities::AppCapabilityClaim {
                    run: run_id,
                    step: &step,
                    message,
                    turn,
                    slot,
                    request,
                    binding_digest: &proof.digest,
                    input_digest: &input_digest,
                    call_id: &call_id,
                },
            )? {
                return Err(Error::unknown("A prior provider call has no retained receipt; its outcome is uncertain and no automatic retry was made."));
            }
            Ok(std::ops::ControlFlow::Continue((step, proof, prepared)))
        })?;
        let (step, proof, prepared) = match prepared {
            std::ops::ControlFlow::Continue(prepared) => prepared,
            std::ops::ControlFlow::Break(existing) => return Ok(existing),
        };
        let human = &expected["approval"]["human"];
        if human.is_object() {
            self.fresh_run_human(human)?;
        }
        let reference = if proof.config["mapping"]["capability"] == "media.generate"
            && proof.config["mapping"]["effect"] == "draft"
            && super::app_runs_rpc::library_reference_inputs_value(&expected["snapshot"]["inputs"])
        {
            Some(self.library_reference_pin(&expected["snapshot"]["inputs"])?)
        } else {
            None
        };
        // Human checks may perform network I/O: re-prove receiver and, for a
        // reference edit, the original pin plus producing installation after
        // them and before sending. Guards are not held across that I/O.
        let admit = |bundle: &Value,
                     files: &BTreeMap<String, String>,
                     producer: Option<(&Value, &BTreeMap<String, String>)>|
         -> Result<Option<Value>> {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let (live_step, live_proof) = current(bundle, files)?;
            if live_step != step || json!(live_proof) != prepared.binding {
                return Err(Error::rejected(
                    "account capability authority changed before dispatch",
                ));
            }
            if let Some(original) = &reference {
                let (producer_bundle, producer_files) = producer
                    .ok_or_else(|| Error::rejected("reference producer snapshot is unavailable"))?;
                return Ok(Some(self.library_reference_under(
                    &expected["snapshot"]["inputs"],
                    original,
                    producer_bundle,
                    producer_files,
                )?));
            }
            Ok(None)
        };
        let edit = if let Some(original) = &reference {
            workspace::with_runtime_snapshot_pair(
                &pm,
                install,
                &original.install_id,
                |(bundle, files), producer| admit(bundle, files, Some(producer)),
            )?
        } else {
            workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
                admit(bundle, files, None)
            })?
        };
        let mut authority = json!({
            "schema":1, "run_id":run_id, "run_snapshot_digest":expected["snapshot_digest"],
            "install_id":install, "context_id":expected["context_id"], "step_id":step,
            "slot":slot, "binding":proof, "inputs":expected["snapshot"]["inputs"],
            "source":expected["snapshot"]["source"], "quote":quote, "call_id":call_id,
        });
        if let Some(edit) = edit {
            authority["reference_edit"] = edit;
            authority["frozen_prompt"] = expected["snapshot"]["inputs"]["instructions"].clone();
        }
        let output = prepared.adapter.execute_app_capability_outcome(
            &prepared.credential, &authority, input, &call_id,
        ).map_err(|error| {
            let reason = if crate::platform::refuse_leak("app capability failure", error.reason(), &prepared.credential).is_ok() {
                error.reason().chars().take(250).collect::<String>()
            } else { "Provider failure details were withheld because they matched credential custody.".into() };
            match error {
                crate::platform::AppCapabilityError::Refused(_) => Error::provider(reason),
                crate::platform::AppCapabilityError::Uncertain(_) => Error::unknown(reason),
            }
        })?;
        let retain = || -> Result<Value> {
            if human.is_object() {
                self.fresh_run_human(human)?;
            }
            let record = |bundle: &Value,
                          files: &BTreeMap<String, String>,
                          producer: Option<(&Value, &BTreeMap<String, String>)>|
             -> Result<Value> {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let (live_step, live_proof) = current(bundle, files)?;
                if live_step != step || json!(live_proof) != prepared.binding {
                    return Err(Error::rejected(
                        "account capability authority changed during dispatch",
                    ));
                }
                if let Some(original) = &reference {
                    let (producer_bundle, producer_files) = producer.ok_or_else(|| {
                        Error::rejected("reference producer snapshot is unavailable")
                    })?;
                    self.library_reference_under(
                        &expected["snapshot"]["inputs"],
                        original,
                        producer_bundle,
                        producer_files,
                    )?;
                }
                crate::platform::refuse_leak(
                    "app capability result",
                    &output.result.to_string(),
                    &prepared.credential,
                )?;
                if let Some(asset) = &output.asset {
                    crate::platform::refuse_leak(
                        "app capability asset",
                        &String::from_utf8_lossy(&asset.bytes),
                        &prepared.credential,
                    )?;
                }
                self.store.app_capability_record(
                    crate::store::app_capabilities::AppCapabilityRecord {
                        id: &call_id,
                        run: run_id,
                        step: &step,
                        message,
                        turn,
                        slot,
                        request,
                        binding_digest: &proof.digest,
                        input_digest: &input_digest,
                        result: &output.result,
                        asset: output
                            .asset
                            .as_ref()
                            .map(|asset| (asset.media_type.as_str(), asset.bytes.as_slice())),
                    },
                )
            };
            if let Some(original) = &reference {
                workspace::with_runtime_snapshot_pair(
                    &pm,
                    install,
                    &original.install_id,
                    |(bundle, files), producer| record(bundle, files, Some(producer)),
                )
            } else {
                workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
                    record(bundle, files, None)
                })
            }
        };
        // Provider work already happened: failed freshness/retention is never
        // reported as known-not-executed, and the claim is never released.
        retain().map_err(|_| Error::unknown("Provider returned a result that could not be safely retained under current authority; the outcome is uncertain and no automatic retry was made."))
    }

    /// Only the existing verified native session can name a browser actor.
    /// Credential-less calls are reachable solely behind operator_connection.
    pub(super) fn account_image_actor(&self, params: &Value) -> Result<Value> {
        if !["token", "key", "origin", "public_session"]
            .iter()
            .any(|key| params.get(key).is_some())
        {
            return Ok(json!({"kind":"operator"}));
        }
        let token = required_str(params, "token")?;
        let key = match params.get("key") {
            None => "",
            Some(Value::String(key)) => key,
            Some(_) => return Err(Error::rejected("native session key must be a string")),
        };
        let origin = required_str(params, "origin")
            .ok()
            .and_then(crate::operator_auth::Origin::parse)
            .ok_or_else(|| Error::rejected("account quote needs a classified native session"))?;
        if let Some(public) = params.get("public_session") {
            if origin != crate::operator_auth::Origin::Public || public.as_str() != Some(token) {
                return Err(Error::rejected(
                    "account quote public session proof differs",
                ));
            }
        }
        let (hash, view) = self
            .tool_session(token, key, origin)
            .ok_or_else(|| Error::rejected("account quote needs a live operator session"))?;
        if origin == crate::operator_auth::Origin::Public
            && !view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator)
        {
            return Err(Error::rejected(
                "account quote needs an operator-role public session",
            ));
        }
        match view.user {
            Some(user) => Ok(json!({"kind":"user","id":user.sub,
                    "issuer":view.scope.as_ref().map(|scope| &scope.issuer),
                    "workspace_id":view.scope.as_ref().map(|scope| &scope.organization_id),"session":hash})),
            None => Ok(json!({"kind":"operator","session":hash})),
        }
    }

    /// Existing mounted tools carry the verified credential hash, not a raw
    /// browser credential. Public hashes still use the central authority gate.
    pub(super) fn account_image_actor_for_session(&self, hash: &str) -> Result<Value> {
        if hash.is_empty() {
            // An assistant's synthetic context is not a native account actor.
            // Existing non-account providers do not consume this identity.
            return Ok(json!({"kind":"unattributed"}));
        }
        let public = self
            .operator_auth()
            .public_snapshot_by_hash(hash, self.operator_now())
            .is_some();
        if public {
            match self.current_session_for_hash(hash)? {
                super::public_membership::Authority::Granted {
                    sub, role, realm, ..
                } if role.is_operator() => Ok(
                    json!({"kind":"user","id":sub,"issuer":realm.issuer,"workspace_id":realm.company,"session":hash}),
                ),
                _ => Err(Error::rejected(
                    "account image mounting session is not a live operator",
                )),
            }
        } else {
            let actor = json!({"kind":"operator","session":hash});
            self.account_image_actor_live(&actor)?;
            Ok(actor)
        }
    }

    pub(super) fn account_image_actor_live(&self, actor: &Value) -> Result<()> {
        if let Some(hash) = actor["session"].as_str() {
            if !self.operator_auth().live_hash(hash, self.operator_now())? {
                return Err(Error::rejected("account quote native session changed"));
            }
        }
        Ok(())
    }

    pub(super) fn account_image_owned(
        actor: &Value,
        source: &crate::platform::account_media::AccountSource,
    ) -> bool {
        actor["kind"] == "operator"
            || (actor["kind"] == "user"
                && actor["id"] == source.subject_id
                && actor["issuer"] == source.issuer
                && actor["workspace_id"] == source.organization_id)
    }

    pub(super) fn app_account_binding_owned(
        &self,
        actor: &Value,
        proof: &BindingProof,
    ) -> Result<()> {
        if proof.config["provider"] == crate::platform::account_media::PLATFORM {
            let source = serde_json::from_value(proof.config["account_source"].clone())
                .map_err(|_| Error::rejected("account binding source identity is invalid"))?;
            if !Self::account_image_owned(actor, &source) {
                return Err(Error::rejected(
                    "saved account does not belong to the proven native actor",
                ));
            }
        }
        Ok(())
    }

    fn app_capability_caller(&self, pid: u32, verb: &str) -> Result<String> {
        let AgentCaller::Agent(alias) = self.agent_caller(pid, verb)? else {
            return Err(Error::rejected("app capability requires an assigned agent"));
        };
        let Some(SlotWho::Strict(proof)) = self.slot_identity(pid)? else {
            return Err(Error::rejected(
                "app capability requires its enrolled managed endpoint",
            ));
        };
        let root = *proof
            .segment
            .last()
            .ok_or_else(|| Error::rejected("app capability endpoint ancestry is empty"))?;
        let caller_session = crate::peer::proc_session(pid)
            .map_err(|_| Error::rejected("app capability caller session is unreadable"))?;
        let endpoint_session = crate::peer::proc_session(root)
            .map_err(|_| Error::rejected("app capability endpoint session is unreadable"))?;
        if proof.lane != alias
            || (caller_session != endpoint_session
                && !self.pi_bash_tool_session(&alias, &proof, caller_session)?)
        {
            return Err(Error::rejected(
                "detached child is outside the assigned app endpoint session",
            ));
        }
        Ok(alias)
    }

    /// Pi's built-in bash tool spawns a direct child of its managed endpoint
    /// with `detached: true`: that shell is a new session leader. Bash may
    /// also exec a single `cadence` command without changing its pid. Admit
    /// only that live, verified direct child session, never a further
    /// `setsid` below the shell or an arbitrary detached program. The
    /// message/turn, binding, quote and one-result checks remain in the RPC.
    pub(super) fn pi_bash_tool_session(
        &self,
        alias: &str,
        proof: &crate::slots::StrictCaller,
        caller_session: u32,
    ) -> Result<bool> {
        use std::os::unix::fs::MetadataExt;

        let agent = self.store.agent(alias)?;
        if !crate::adapter::registry::pi_family_managed(&agent.provider, &agent.endpoint_kind) {
            return Ok(false);
        }
        let Some(&tool_pid) = proof.segment.get(proof.segment.len().saturating_sub(2)) else {
            return Ok(false);
        };
        if caller_session != tool_pid || crate::peer::proc_session(tool_pid).ok() != Some(tool_pid)
        {
            return Ok(false);
        }
        let Ok(tool) = std::fs::metadata(format!("/proc/{tool_pid}/exe")) else {
            return Ok(false);
        };
        // Compare the executable's inode, not its caller-controlled basename.
        // The exact daemon binary covers bash's last-command exec optimization.
        let trusted = ["/bin/bash", "/bin/sh", "/proc/self/exe"];
        Ok(trusted.iter().any(|path| {
            std::fs::metadata(path)
                .is_ok_and(|expected| expected.dev() == tool.dev() && expected.ino() == tool.ino())
        }))
    }

    pub(super) fn rpc_app_capability(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        pid: u32,
    ) -> Result<Value> {
        match method {
            "app_binding_quote" => {
                strict_fields(
                    params,
                    &[
                        "install_id",
                        "context_id",
                        "slot",
                        "token",
                        "key",
                        "origin",
                        "public_session",
                    ],
                )?;
                self.operator_connection("app capability quote", params, pid)?;
                let install = required_str(params, "install_id")?;
                let slot = required_str(params, "slot")?;
                let context = optional_str(params, "context_id");
                let actor = self.account_image_actor(params)?;
                let pm = self.pm_at(&self.pm_dir()?)?;
                let (proof, request) =
                    workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
                        let _custody = self
                            .platform_custody_lock
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let _release = self
                            .app_release_lock
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let proof = self
                            .app_binding_auto_image(install, context, slot, &actor, bundle, files)?
                            .ok_or_else(|| Error::rejected("capability binding is absent"))?;
                        let request = self.prepare_app_capability_quote(&proof)?;
                        Ok((proof, request))
                    })?;
                let quote = request.fetch()?;
                if self.account_image_actor(params)? != actor {
                    return Err(Error::rejected("capability quote native actor changed"));
                }
                workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
                    let _custody = self.custody_operation()?;
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    self.account_image_actor_live(&actor)?;
                    self.app_account_binding_owned(&actor, &proof)?;
                    self.app_binding_receipt_current(
                        install, context, slot, &proof, bundle, files,
                    )?;
                    self.recheck_app_capability_quote(&proof, &quote)?;
                    Ok(json!({"slot":slot,"binding_digest":proof.digest,
                        "quote":quote,"quote_digest":app_runs::material_digest(&json!(quote))}))
                })
            }
            "app_run_capability_call" => {
                strict_fields(params, &["message", "token", "slot", "request_id", "input"])?;
                let alias = self.app_capability_caller(pid, "app capability call")?;
                let message = required_str(params, "message")?;
                let turn = required_str(params, "token")?;
                let slot =
                    crate::proto::identifier(required_str(params, "slot")?, "Capability slot")?;
                let request = crate::proto::identifier(
                    required_str(params, "request_id")?,
                    "Capability request",
                )?;
                let input = params
                    .get("input")
                    .ok_or_else(|| Error::rejected("capability input is required"))?;
                if !input.is_object() || serde_json::to_vec(input)?.len() > 64 * 1024 {
                    return Err(Error::rejected(
                        "capability input must be a JSON object within 64 KiB",
                    ));
                }
                let msg = self
                    .store
                    .message(message)?
                    .filter(|m| {
                        m.alias == alias
                            && m.source == "app_run_dispatch"
                            && m.state == "running"
                            && m.turn_id.as_deref() == Some(turn)
                    })
                    .ok_or_else(|| {
                        Error::rejected("capability needs the active assigned app turn")
                    })?;
                let (run_id, install) = self
                    .store
                    .app_message_installation(&msg.id)?
                    .ok_or_else(|| Error::rejected("capability app association is absent"))?;
                let checked_run = self.store.app_run_show(&run_id)?;
                // The next probe prices every frozen slot, not only the one
                // the worker named. Lineage is offline and ignores non-account
                // slots, so an unknown or mixed slot cannot skip it.
                self.account_frozen_lineage(&checked_run)?;
                self.probe_app_run_prices(&checked_run)?;
                let quote = self
                    .prepare_app_run_quotes(&checked_run)?
                    .remove(&slot)
                    .ok_or_else(|| Error::rejected("run capability quote is absent"))??
                    .fetch()?;
                if checked_run["snapshot"]["capabilities"][&slot]["config"]["provider"]
                    == crate::platform::account_media::PLATFORM
                {
                    return self.execute_account_run_capability(
                        &checked_run,
                        &slot,
                        &quote,
                        (input, &request),
                        Some((&alias, message, turn)),
                    );
                }
                let pm = self.pm_at(&self.pm_dir()?)?;
                // Claim under the runtime/custody/release guards; provider I/O
                // remains outside them, followed by a fresh result check.
                let prepared = workspace::with_runtime_snapshot(&pm, &install, |bundle, files| {
                    let _custody = self.custody_operation()?;
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let run = self.store.app_run_show(&run_id)?;
                    self.app_run_binding_current(&run, bundle, files)?;
                    let (run, step, proof) = self.store.app_capability_turn(
                        &alias,
                        message,
                        turn,
                        &slot,
                        required_str(bundle, "digest")?,
                    )?;
                    // CAD-1374: the sealed approving human bound at approval
                    // time. Null for a legacy approval (no human recorded).
                    let sealed_human = run["approval"]["human"].clone();
                    if run["snapshot_digest"] != checked_run["snapshot_digest"]
                        || run["snapshot"]["quotes"][&slot] != json!(quote)
                    {
                        return Err(Error::rejected(
                            "capability price changed since run approval",
                        ));
                    }
                    self.recheck_app_capability_quote(&proof, &quote)?;
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
                    let input_digest = app_runs::material_digest(input);
                    if let Some(existing) =
                        self.store.app_capability_result_for_slot(&run_id, &slot)?
                    {
                        if existing["step_id"] != step
                            || existing["request_id"] != request
                            || existing["input_digest"] != input_digest
                            || existing["binding_digest"] != proof.digest
                        {
                            return Err(Error::rejected(
                                "approved run capability slot already has its one result",
                            ));
                        }
                        return Ok(std::ops::ControlFlow::Break(existing));
                    }
                    let call_id = format!(
                        "app-call-{}",
                        uuid::Uuid::new_v5(
                            &uuid::Uuid::NAMESPACE_OID,
                            format!("{run_id}:{slot}").as_bytes(),
                        )
                        .simple()
                    );
                    // The claim is the exactly-once gate: a second identical
                    // request makes no provider call and must not fail the run
                    // (the first call may still be in flight).
                    if !self.store.app_capability_claim(
                        crate::store::app_capabilities::AppCapabilityClaim {
                            run: &run_id,
                            step: &step,
                            message,
                            turn,
                            slot: &slot,
                            request: &request,
                            binding_digest: &proof.digest,
                            input_digest: &input_digest,
                            call_id: &call_id,
                        },
                    )? {
                        return Err(Error::unknown("A prior provider call has no retained receipt; its outcome is uncertain and no automatic retry was made."));
                    }
                    let authority = json!({
                        "schema":1,"run_id":run_id,"run_snapshot_digest":run["snapshot_digest"],
                        "install_id":install,"context_id":run["context_id"],
                        "step_id":step,"slot":slot,"binding":proof,
                        "inputs":run["snapshot"]["inputs"],"source":run["snapshot"]["source"],
                        "quote":run["snapshot"]["quotes"][&slot],
                        "call_id":call_id,
                    });
                    let credential = self.app_capability_credential(config)?;
                    // CAD-1374: a NEW external call re-proves the approving
                    // human after the durable claim and before any provider
                    // bytes leave. A revoked/demoted human refuses here; the
                    // claim stays as an honest prepared intent, never resent.
                    if sealed_human.is_object() {
                        self.fresh_run_human(&sealed_human)?;
                    }
                    // Re-read the live run and active assigned turn after the
                    // fresh I/O. The approval human binding and the turn must
                    // still be the ones proven above.
                    let (current, current_step, current_proof) = self.store.app_capability_turn(
                        &alias,
                        message,
                        turn,
                        &slot,
                        required_str(bundle, "digest")?,
                    )?;
                    if current["approval"]["human"] != sealed_human
                        || current_step != step
                        || current_proof.digest != proof.digest
                    {
                        return Err(Error::rejected(
                            "capability approval or turn changed before the external call",
                        ));
                    }
                    Ok(std::ops::ControlFlow::Continue((
                        std::sync::Arc::clone(adapter),
                        credential,
                        authority,
                        call_id,
                        input_digest,
                        step,
                        proof,
                        sealed_human,
                    )))
                })?;
                let (
                    adapter,
                    credential,
                    authority,
                    call_id,
                    input_digest,
                    step,
                    proof,
                    sealed_human,
                ) = match prepared {
                    std::ops::ControlFlow::Continue(prepared) => prepared,
                    std::ops::ControlFlow::Break(existing) => return Ok(existing),
                };
                crate::issue::lockseam::outside_lock("app-capability-provider-call");
                let output = adapter
                    .execute_app_capability_outcome(&credential, &authority, input, &call_id)
                    .map_err(|error| {
                        let reason = if crate::platform::refuse_leak(
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
                        let _ = self
                            .store
                            .app_run_call_failed(&run_id, &step, error.kind(), &reason);
                        match error {
                            crate::platform::AppCapabilityError::Refused(_) => {
                                Error::provider(reason)
                            }
                            crate::platform::AppCapabilityError::Uncertain(_) => {
                                Error::unknown(reason)
                            }
                        }
                    })?;
                // CAD-1258 interleaving point: a revoke or upgrade may land here.
                crate::issue::lockseam::outside_lock("app-capability-before-record");
                let recorded = workspace::with_runtime_snapshot(&pm, &install, |bundle, files| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    self.app_run_binding_current(&self.store.app_run_show(&run_id)?, bundle, files)
                        .map_err(super::app_effects_rpc::lock2_authority)?;
                    let (current, current_step, current_proof) = self
                        .store
                        .app_capability_turn(
                            &alias,
                            message,
                            turn,
                            &slot,
                            required_str(bundle, "digest")?,
                        )
                        .map_err(super::app_effects_rpc::lock2_authority)?;
                    if current["approval"]["human"] != sealed_human
                        || current_step != step
                        || current_proof.digest != proof.digest
                    {
                        return Err(super::app_effects_rpc::lock2_authority(Error::rejected(
                            "capability approval or turn changed during the external call",
                        )));
                    }
                    crate::platform::refuse_leak(
                        "app capability result",
                        &output.result.to_string(),
                        &credential,
                    )?;
                    if let Some(asset) = &output.asset {
                        crate::platform::refuse_leak(
                            "app capability asset",
                            &String::from_utf8_lossy(&asset.bytes),
                            &credential,
                        )?;
                    }
                    self.store
                        .app_capability_record(
                            crate::store::app_capabilities::AppCapabilityRecord {
                                id: &call_id,
                                run: &run_id,
                                step: &step,
                                message,
                                turn,
                                slot: &slot,
                                request: &request,
                                binding_digest: &proof.digest,
                                input_digest: &input_digest,
                                result: &output.result,
                                asset: output.asset.as_ref().map(|asset| {
                                    (asset.media_type.as_str(), asset.bytes.as_slice())
                                }),
                            },
                        )
                        .map_err(|error| Error::internal(error.to_string()))
                });
                if let Err(error) = &recorded {
                    // The provider answered but lock 2 refused or could not
                    // retain it: leave a durable uncertain failure naming
                    // the cause, never a silent drop. The claim blocks any
                    // resend. If that write fails too, say so.
                    let reason = super::app_effects_rpc::lock2_failure_reason(
                        error,
                        &[credential.as_slice()],
                    );
                    if let Err(write) =
                        self.store
                            .app_run_call_failed(&run_id, &step, "uncertain", &reason)
                    {
                        return Err(Error::unknown(format!(
                            "{reason} (the failure could not be recorded: {})",
                            super::app_effects_rpc::lock2_cause(&write, &[credential.as_slice()])
                        )));
                    }
                    // The caller gets the same screened reason, never the raw
                    // lock-2 error text.
                    return Err(Error::unknown(reason));
                }
                recorded
            }
            "app_run_capability_results"
            | "app_run_capability_result"
            | "app_run_capability_asset" => {
                strict_fields(
                    params,
                    if method == "app_run_capability_results" {
                        &["run_id"]
                    } else {
                        &["receipt_id", "message", "token"]
                    },
                )?;
                if let Some(message) = optional_str(params, "message") {
                    if method == "app_run_capability_results" {
                        return Err(Error::rejected("worker cannot list capability results"));
                    }
                    let token = required_str(params, "token")?;
                    let alias = self.app_capability_caller(pid, "app capability receipt")?;
                    let receipt_id = required_str(params, "receipt_id")?;
                    let receipt = self.store.app_capability_result(receipt_id)?;
                    let run = self.store.app_run_show(required_str(&receipt, "run_id")?)?;
                    self.probe_app_run_prices(&run)?;
                    let pm = self.pm_at(&self.pm_dir()?)?;
                    return workspace::with_runtime_snapshot(
                        &pm,
                        required_str(&run, "install_id")?,
                        |bundle, files| {
                            let _custody = self.custody_operation()?;
                            let _release = self
                                .app_release_lock
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            self.app_run_binding_current(&run, bundle, files)?;
                            self.store.app_capability_receipt_for_turn(
                                receipt_id,
                                &alias,
                                message,
                                token,
                                required_str(bundle, "digest")?,
                            )?;
                            if method == "app_run_capability_asset" {
                                self.store.app_capability_asset(receipt_id)
                            } else {
                                Ok(receipt)
                            }
                        },
                    );
                }
                if params.get("token").is_some() {
                    return Err(Error::rejected("token requires its assigned message"));
                }
                self.operator_connection("app capability receipt", params, pid)?;
                // Historical operator reads must not turn an unsupported
                // encrypted binding into a cache-return bypass. Only classify
                // the stored routing targets: no provider quote, credential
                // load, execution grant, or legacy historical revalidation.
                let _custody = self.custody_operation()?;
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let run_id = if method == "app_run_capability_results" {
                    required_str(params, "run_id")?.to_string()
                } else {
                    let receipt = self
                        .store
                        .app_capability_result(required_str(params, "receipt_id")?)
                        .map_err(|error| match error {
                            Error::Rejected(message)
                                if method == "app_run_capability_asset"
                                    && message == "app capability result unavailable" =>
                            {
                                Error::rejected("app capability asset unavailable")
                            }
                            other => other,
                        })?;
                    required_str(&receipt, "run_id")?.to_string()
                };
                let run = self.store.app_run_show(&run_id)?;
                if let Some(capabilities) = run["snapshot"]["capabilities"].as_object() {
                    for value in capabilities.values() {
                        let proof: crate::store::app_bindings::BindingProof =
                            serde_json::from_value(value.clone()).map_err(|_| {
                                Error::rejected("run capability binding receipt is invalid")
                            })?;
                        self.refuse_encrypted_binding(&proof.config)?;
                    }
                }
                match method {
                    "app_run_capability_results" => self
                        .store
                        .app_capability_results(required_str(params, "run_id")?),
                    "app_run_capability_result" => self
                        .store
                        .app_capability_result(required_str(params, "receipt_id")?),
                    _ => self
                        .store
                        .app_capability_asset(required_str(params, "receipt_id")?),
                }
            }
            _ => Err(Error::rejected("unknown app capability method")),
        }
    }
}

#[cfg(all(test, unix, feature = "test-seam"))]
mod early_app_quote_tests;
