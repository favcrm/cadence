//! Context-scoped social draft operations through a live, server-minted screen action.

#[cfg(test)]
#[path = "app_selector_identity_acceptance.rs"]
mod app_selector_identity_acceptance;
#[cfg(all(test, feature = "test-seam"))]
#[path = "app_social_discard_gate_test.rs"]
mod app_social_discard_gate_test;
#[cfg(test)]
#[path = "app_social_local_authority_acceptance.rs"]
mod app_social_local_authority_acceptance;
use super::{app_bindings_rpc::strict_fields, app_tools_rpc::ToolContext, required_str, Shared};
use crate::issue::app_catalog::workspace;
use crate::{
    error::{Error, Result},
    operator_auth::Origin,
    store::{app_records::RecordStore, app_social_drafts::DraftSource},
};
use serde_json::{json, Value};

fn origin(params: &Value) -> Result<Origin> {
    match params["origin"].as_str() {
        Some("loopback") => Ok(Origin::Loopback),
        Some("tailnet") => Ok(Origin::Tailnet),
        Some("public") => Ok(Origin::Public),
        _ => Err(Error::rejected(
            "social draft action needs a classified session origin",
        )),
    }
}
fn local_social_draft_write(method: &str) -> Result<bool> {
    match method {
        "app_social_draft_list" | "app_social_draft_show" | "app_social_sources_show" => Ok(false),
        "app_social_draft_asset"
        | "app_social_draft_create"
        | "app_social_draft_update"
        | "app_social_draft_discard"
        | "app_social_sources_save" => Ok(true),
        _ => Err(Error::rejected("unknown social draft action")),
    }
}

#[derive(Clone, Copy)]
enum BindingRequirement {
    Required,
    LocalPersistence,
}

fn source(params: &Value) -> Result<DraftSource> {
    let s = params
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::rejected("social draft source must be an object"))?;
    let kind = s.get("kind").and_then(Value::as_str).unwrap_or("");
    if s.keys()
        .any(|k| !matches!(k.as_str(), "kind" | "receipt_id" | "run_id" | "post_id"))
    {
        return Err(Error::rejected(
            "social draft source has unsupported fields",
        ));
    }
    let post_id = s.get("post_id").and_then(Value::as_str).map(str::to_owned);
    match kind {
        "tool_receipt"
            if s.get("receipt_id").and_then(Value::as_str).is_some()
                && !s.contains_key("run_id") =>
        {
            Ok(DraftSource::ToolReceipt {
                receipt_id: s["receipt_id"].as_str().unwrap().to_owned(),
                post_id,
            })
        }
        "run"
            if s.get("run_id").and_then(Value::as_str).is_some()
                && s.get("receipt_id").and_then(Value::as_str).is_some() =>
        {
            Ok(DraftSource::Run {
                run_id: s["run_id"].as_str().unwrap().to_owned(),
                receipt_id: s["receipt_id"].as_str().unwrap().to_owned(),
                post_id,
            })
        }
        _ => Err(Error::rejected(
            "social draft source must name one real receipt or run",
        )),
    }
}

impl Shared {
    pub(super) fn social_draft_action(
        &self,
        params: &Value,
        peer_pid: u32,
        write: bool,
    ) -> Result<(ToolContext, String)> {
        self.social_draft_action_with_binding(params, peer_pid, write, BindingRequirement::Required)
    }

    fn social_draft_action_with_binding(
        &self,
        params: &Value,
        peer_pid: u32,
        write: bool,
        binding_requirement: BindingRequirement,
    ) -> Result<(ToolContext, String)> {
        self.operator_connection("social draft action", params, peer_pid)?;
        let o = origin(params)?;
        let (session, view) = self
            .tool_session(
                required_str(params, "token")?,
                params.get("key").and_then(Value::as_str).unwrap_or(""),
                o,
            )
            .ok_or_else(|| {
                Error::rejected("social draft action needs a live operator session — sign in again")
            })?;
        if o == Origin::Public
            && !view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator)
        {
            return Err(Error::rejected(
                "social draft action needs an operator-role public session",
            ));
        }
        let ctx = self.tool_context_proven(required_str(params, "action_token")?, &session)?;
        let context = ctx
            .context_id
            .clone()
            .ok_or_else(|| Error::rejected("social draft action needs a mounted active context"))?;
        let alias = required_str(params, "tool_alias")?;
        let slot = ctx.tools.get(alias).ok_or_else(|| {
            Error::rejected("draft action alias is not declared by the live screen")
        })?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, &ctx.install_id, |bundle, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if required_str(bundle, "digest")? != ctx.digest {
                return Err(Error::rejected("installation digest changed since mount"));
            }
            if self
                .store
                .app_capability_status(&ctx.install_id, &ctx.digest)?["state"]
                != "approved"
            {
                return Err(Error::rejected(
                    "installation is not approved at its current digest",
                ));
            }
            let manifest = crate::issue::app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?;
            let declaration = manifest.capabilities.get(slot).ok_or_else(|| {
                Error::rejected("draft slot is not declared by this installation")
            })?;
            declaration.validate()?;
            if (write && declaration.effect != "draft")
                || (!write && !matches!(declaration.effect.as_str(), "read" | "draft"))
            {
                return Err(Error::rejected(
                    "social draft write requires a declared draft-effect slot",
                ));
            }
            if matches!(binding_requirement, BindingRequirement::Required) {
                let binding = self
                    .app_binding_live(&ctx.install_id, Some(&context), slot, bundle, files)?
                    .ok_or_else(|| Error::rejected("social draft capability binding is absent"))?;
                if binding.config["mapping"]["effect"] != declaration.effect {
                    return Err(Error::rejected(
                        "social draft binding effect differs from its declaration",
                    ));
                }
            }
            Ok(())
        })?;
        self.store.app_context_proof(&ctx.install_id, &context)?;
        Ok((ctx, context))
    }

    pub(super) fn verify_source(
        &self,
        install: &str,
        context: &str,
        source: &DraftSource,
        asset_id: Option<&str>,
    ) -> Result<()> {
        match source {
            DraftSource::ToolReceipt {
                receipt_id,
                post_id,
            } => {
                let records = RecordStore::open(&self.state_dir, install)?;
                if !records.app_social_tool_receipt_contains(context, receipt_id)? {
                    return Err(Error::rejected(
                        "source receipt is not retained in this context",
                    ));
                }
                if !records.app_social_freshness_contains(context, receipt_id)? {
                    return Err(Error::rejected(
                        "source receipt is not the latest successful fetch for a saved handle",
                    ));
                }
                let post = post_id
                    .as_deref()
                    .ok_or_else(|| Error::rejected("selected social source needs a post ID"))?;
                if !self
                    .store
                    .app_tool_source_post_exists(install, receipt_id, post)?
                {
                    return Err(Error::rejected(
                        "source receipt or post is not retained for this installation",
                    ));
                }
            }
            DraftSource::Run {
                run_id,
                receipt_id,
                post_id,
            } => {
                let post = post_id
                    .as_deref()
                    .ok_or_else(|| Error::rejected("selected social source needs a post ID"))?;
                let run = self.store.app_run_show(run_id)?;
                if run["install_id"] != install || run["context_id"] != context {
                    return Err(Error::rejected(
                        "source run does not belong to this installation and context",
                    ));
                }
                let _caption = self.store.app_selected_source_input(
                    install,
                    Some(context),
                    receipt_id,
                    post,
                )?;
                let receipt = self.store.app_capability_result(receipt_id)?;
                if receipt["run_id"].as_str() != Some(run_id.as_str()) {
                    return Err(Error::rejected(
                        "selected source receipt does not belong to the named run",
                    ));
                }
            }
        }
        if let Some(asset) = asset_id {
            let records = RecordStore::open(&self.state_dir, install)?;
            if !records.app_social_tool_receipt_contains(context, asset)? {
                return Err(Error::rejected(
                    "image receipt is not retained in this context",
                ));
            }
            let (header, digest, bytes) = self.store.app_tool_asset_bytes(install, asset)?;
            let mime = crate::platform::agenticos_external::image::image_mime(&bytes, &header)
                .map_err(Error::rejected)?;
            if !matches!(mime, "image/jpeg" | "image/png")
                || bytes.len() > 2 * 1024 * 1024
                || image::load_from_memory(&bytes).is_err()
                || crate::store::app_runs::artifact_digest(&bytes) != digest
            {
                return Err(Error::rejected(
                    "social draft attachment must be an intact owned JPEG or PNG",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn rpc_app_social_draft(
        &self,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let allowed = match method {
            "app_social_draft_create" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "request_id",
                "caption",
                "source",
                "asset_id",
            ][..],
            "app_social_draft_list" => {
                &["action_token", "token", "key", "origin", "tool_alias"][..]
            }
            "app_social_draft_show" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "draft_id",
            ][..],
            "app_social_draft_update" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "request_id",
                "draft_id",
                "expected_revision",
                "caption",
                "asset_id",
            ][..],
            "app_social_draft_discard" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "draft_id",
                "revision",
            ][..],
            "app_social_draft_asset" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "draft_id",
            ][..],
            "app_social_sources_show" => {
                &["action_token", "token", "key", "origin", "tool_alias"][..]
            }
            "app_social_sources_save" => &[
                "action_token",
                "token",
                "key",
                "origin",
                "tool_alias",
                "request_id",
                "expected_revision",
                "handles",
            ][..],
            _ => return Err(Error::rejected("unknown social draft action")),
        };
        strict_fields(params, allowed)?;
        // This flag means the slot must declare draft effect, including for
        // asset reads; it is not merely an I/O-write classification.
        let write = local_social_draft_write(method)?;
        // The action context/session/mount and declared live capability are
        // proved before the RecordStore is opened. Local persistence does not
        // dispatch or bill; effect execution still requires its live binding.
        let (ctx, context) = self.social_draft_action_with_binding(
            params,
            peer_pid,
            write,
            BindingRequirement::LocalPersistence,
        )?;
        let _draft_release = if matches!(
            method,
            "app_social_draft_create"
                | "app_social_draft_update"
                | "app_social_draft_discard"
                | "app_social_sources_save"
        ) {
            Some(
                self.app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
            )
        } else {
            None
        };
        let records = RecordStore::open(&self.state_dir, &ctx.install_id)?;
        match method {
            "app_social_draft_list" => {
                let mut result = records.app_social_draft_list(&context)?;
                let mut intents = records.app_social_generation_intents(&context)?;
                for intent in &mut intents {
                    let request = intent["request_id"].as_str().unwrap_or("").to_owned();
                    let state = intent["state"].as_str().unwrap_or("").to_owned();
                    let receipt = if matches!(state.as_str(), "pending" | "uncertain") {
                        self.store.app_tool_result_for_request(&request)?
                    } else if state == "completed" {
                        intent["receipt_id"]
                            .as_str()
                            .map(|id| self.store.app_tool_result(id))
                            .transpose()?
                    } else {
                        None
                    };
                    if let Some(receipt) = receipt {
                        records
                            .app_social_generation_validate_receipt(&context, &request, &receipt)?;
                        let id = required_str(&receipt, "id")?;
                        let completed = receipt["created_at"].as_f64().unwrap_or(0.0);
                        if matches!(state.as_str(), "pending" | "uncertain") {
                            records.app_social_generation_complete_request(&request, id)?;
                            records.app_social_tool_receipt_attach(&context, id, completed)?;
                            intent["state"] = json!("completed");
                            intent["receipt_id"] = json!(id);
                        }
                        intent["receipt"] = receipt;
                    }
                }
                result["generation_intents"] = json!(intents);
                result["effects"] = records.app_social_effect_list(&context)?["effects"].clone();
                Ok(result)
            }
            "app_social_sources_show" => records.app_social_sources_show(&context),
            "app_social_sources_save" => {
                let handles = params
                    .get("handles")
                    .and_then(Value::as_array)
                    .ok_or_else(|| Error::rejected("social source handles must be an array"))?;
                let handles = handles
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| Error::rejected("social source handle must be a string"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                records.app_social_sources_save(
                    &context,
                    params
                        .get("expected_revision")
                        .and_then(Value::as_i64)
                        .ok_or_else(|| Error::rejected("expected_revision must be an integer"))?,
                    &handles,
                    required_str(params, "request_id")?,
                )
            }
            "app_social_draft_show" => {
                records.app_social_draft_show(&context, required_str(params, "draft_id")?)
            }
            "app_social_draft_asset" => {
                let draft =
                    records.app_social_draft_show(&context, required_str(params, "draft_id")?)?;
                let asset = draft["asset_id"]
                    .as_str()
                    .ok_or_else(|| Error::rejected("social draft has no attached image"))?;
                let (header, digest, bytes) =
                    self.store.app_tool_asset_bytes(&ctx.install_id, asset)?;
                let mime = crate::platform::agenticos_external::image::image_mime(&bytes, &header)
                    .map_err(Error::rejected)?;
                if !matches!(mime, "image/jpeg" | "image/png") {
                    return Err(Error::rejected("publish image must be JPEG or PNG"));
                }
                if crate::store::app_runs::artifact_digest(&bytes) != digest {
                    return Err(Error::rejected("social image custody digest changed"));
                }
                Ok(
                    json!({"install_id":ctx.install_id,"draft_id":draft["draft_id"],"revision":draft["revision"],"asset_id":asset,"mime":mime,"digest":digest,"size_bytes":bytes.len()}),
                )
            }
            "app_social_draft_create" => {
                let source = source(params)?;
                let asset = params.get("asset_id").and_then(Value::as_str);
                self.verify_source(&ctx.install_id, &context, &source, asset)?;
                let actor = format!("session:{}", &ctx.session[..12]);
                records.app_social_draft_create(
                    &context,
                    required_str(params, "caption")?,
                    &source,
                    asset,
                    required_str(params, "request_id")?,
                    &actor,
                )
            }
            "app_social_draft_discard" => records.app_social_draft_discard(
                &context,
                required_str(params, "draft_id")?,
                params["revision"]
                    .as_i64()
                    .ok_or_else(|| Error::rejected("revision must be an integer"))?,
            ),
            "app_social_draft_update" => {
                let asset = match params.get("asset_id") {
                    None => None,
                    Some(Value::Null) => Some(None),
                    Some(Value::String(s)) => Some(Some(s.as_str())),
                    _ => return Err(Error::rejected("asset_id must be a string or null")),
                };
                let old =
                    records.app_social_draft_show(&context, required_str(params, "draft_id")?)?;
                let src: DraftSource = serde_json::from_value(old["source"].clone())
                    .map_err(|_| Error::rejected("social draft provenance is corrupt"))?;
                let new_asset = asset.unwrap_or(old["asset_id"].as_str());
                self.verify_source(&ctx.install_id, &context, &src, new_asset)?;
                let actor = format!("session:{}", &ctx.session[..12]);
                records.app_social_draft_update(
                    &context,
                    required_str(params, "draft_id")?,
                    &crate::store::app_social_drafts::SocialDraftEdit {
                        expected: params["expected_revision"].as_i64().ok_or_else(|| {
                            Error::rejected("expected_revision must be an integer")
                        })?,
                        caption: required_str(params, "caption")?,
                        asset_id: asset,
                        request_id: required_str(params, "request_id")?,
                        actor: &actor,
                    },
                )
            }
            _ => Err(Error::rejected("unknown social draft action")),
        }
    }
}
