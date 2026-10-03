//! Run-bound read/draft broker. A connection-derived agent proves its exact
//! active app turn; installation, context, action and account are frozen.
use super::app_bindings_rpc::strict_fields;
use super::*;
use crate::contract_fixture::{classify_call, Effect};
use crate::issue::app_catalog::workspace;
use crate::store::app_bindings::BindingProof;
use crate::store::app_runs;

const PRICE_REFUSED: &str = "bound capability price discovery refused";

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
    fn app_capability_credential(&self, config: &Value) -> Result<Vec<u8>> {
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

    pub(super) fn app_capability_quote(
        &self,
        proof: &BindingProof,
    ) -> Result<crate::platform::AppCapabilityQuote> {
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
        let credential = self.app_capability_credential(config)?;
        let quote = adapter
            .quote_app_capability(&credential, &serde_json::to_value(proof)?)
            .map_err(|error| Error::rejected(price_refusal(&error)))?;
        if !quote.valid() {
            return Err(Error::rejected("bound capability price quote is invalid"));
        }
        Ok(quote)
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
        if agent.provider != "pi" || agent.endpoint_kind != "managed" {
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
                strict_fields(params, &["install_id", "context_id", "slot"])?;
                self.operator_connection("app capability quote", params, pid)?;
                let install = required_str(params, "install_id")?;
                let slot = required_str(params, "slot")?;
                let context = optional_str(params, "context_id");
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
                    let proof = self
                        .app_binding_live(install, context, slot, bundle, files)?
                        .ok_or_else(|| Error::rejected("capability binding is absent"))?;
                    let quote = self.app_capability_quote(&proof)?;
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
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(&pm, &install, |bundle, files| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
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
                    let quote = self.app_capability_quote(&proof)?;
                    if run["snapshot"]["quotes"][&slot] != json!(quote) {
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
                        return Ok(existing);
                    }
                    let call_id = format!(
                        "app-call-{}",
                        uuid::Uuid::new_v5(
                            &uuid::Uuid::NAMESPACE_OID,
                            format!("{run_id}:{slot}").as_bytes(),
                        )
                        .simple()
                    );
                    self.store.app_capability_claim(
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
                    )?;
                    let authority = json!({
                        "schema":1,"run_id":run_id,"run_snapshot_digest":run["snapshot_digest"],
                        "install_id":install,"context_id":run["context_id"],
                        "step_id":step,"slot":slot,"binding":proof,
                        "inputs":run["snapshot"]["inputs"],"source":run["snapshot"]["source"],
                        "quote":run["snapshot"]["quotes"][&slot],
                        "call_id":call_id,
                    });
                    let credential = self.app_capability_credential(config)?;
                    let output = adapter
                        .execute_app_capability(&credential, &authority, input, &call_id)
                        .map_err(Error::rejected)?;
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
                    self.store.app_capability_record(
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
                            asset: output
                                .asset
                                .as_ref()
                                .map(|asset| (asset.media_type.as_str(), asset.bytes.as_slice())),
                        },
                    )
                })
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
                    let pm = self.pm_at(&self.pm_dir()?)?;
                    return workspace::with_runtime_snapshot(
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
