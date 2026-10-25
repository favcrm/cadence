//! CAD-1184 generic CRM assistant RPCs. Agent calls use only the existing
//! live-turn verifier; operator decision calls prove the operator connection.
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

use super::{required_str, Shared};
use crate::app_assistant::Declared;
use crate::error::{Error, Result};
use crate::issue::app_catalog::workspace;
use crate::store::app_audiences::AudienceBase;
use crate::store::app_records::{AssistantOperationUpdate, AssistantPermissionWrite, RecordStore};
use crate::store::app_sends::send_view;

fn exact(params: &Value, allowed: &[&str]) -> Result<()> {
    let fields = params
        .as_object()
        .ok_or_else(|| Error::rejected("assistant RPC payload must be an object"))?;
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::rejected(
            "assistant RPC payload has unsupported fields",
        ));
    }
    Ok(())
}

/// The installation's runtime digest, refused unless the operator approved
/// exactly that digest.
fn consented_digest(shared: &Shared, install: &str, row: &Value) -> Result<String> {
    let digest = row
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("installation digest unavailable"))?;
    let state = shared.store.app_capability_status(install, digest)?;
    if state.get("state").and_then(Value::as_str) != Some("approved") {
        return Err(Error::rejected("app assistant descriptor is not consented"));
    }
    Ok(digest.to_string())
}

fn install_descriptor(shared: &Shared, install: &str) -> Result<(String, Value)> {
    let pm = shared.pm_at(&shared.pm_dir()?)?;
    workspace::with_runtime_read(&pm, install, |row, files| {
        let digest = consented_digest(shared, install, row)?;
        let text = files
            .get(crate::issue::app_assistant::FILE)
            .ok_or_else(|| Error::rejected("installation has no assistant actions"))?;
        Ok((digest, crate::issue::app_assistant::size_and_json(text)?))
    })
}

/// CAD-1518: what the installation's consented bundle declares — its
/// registered assistant actions and its host chat attachments. A missing
/// file declares nothing; an unconsented digest refuses.
pub(super) fn install_declared(shared: &Shared, install: &str) -> Result<Declared> {
    let pm = shared.pm_at(&shared.pm_dir()?)?;
    workspace::with_runtime_read(&pm, install, |row, files| {
        consented_digest(shared, install, row)?;
        let manifest = crate::issue::app::parse_manifest(
            files
                .get("app.md")
                .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
        )?;
        let ids = |text: Option<&String>, parse: fn(&str) -> Result<Value>, key: &str| {
            let Some(text) = text else {
                return Ok(BTreeSet::new());
            };
            Ok::<_, Error>(
                parse(text)?[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|row| row["id"].as_str().map(str::to_string))
                    .collect(),
            )
        };
        Ok(Declared {
            actions: ids(
                files.get(crate::issue::app_assistant::FILE),
                crate::issue::app_assistant::size_and_json,
                "actions",
            )?,
            attachments: ids(
                files.get(crate::issue::app_chat::FILE),
                crate::issue::app_chat::size_and_json,
                "attachments",
            )?,
            app: manifest.app,
            title: manifest.title,
        })
    })
}

/// The registered Social actions that run a paid provider call behind a
/// one-time operator permission.
fn is_paid_social(action: &str) -> bool {
    matches!(action, "social.posts.fetch" | "social.draft.create")
}

pub(super) fn assistant_error_is_definite_refusal(error: &Error) -> bool {
    match error {
        Error::Rejected(_) => true,
        Error::Structured(details) => matches!(details.kind, "rejected" | "conflict" | "gate"),
        _ => false,
    }
}

fn normalize_interrupted_operation(
    records: &RecordStore,
    context: &str,
    mut operation: Value,
) -> Result<Value> {
    if operation.get("status").and_then(Value::as_str) == Some("running") {
        let operation_id = required_str(&operation, "id")?.to_string();
        let revision = operation
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::internal("assistant operation revision is unavailable"))?;
        let message =
            "Execution interrupted; outcome unknown. Verify the affected record before retrying.";
        let result = operation.get("result").cloned().unwrap_or(Value::Null);
        let resource_refs = operation
            .get("resource_refs")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let permission_request = operation
            .get("permission_request")
            .cloned()
            .unwrap_or(Value::Null);
        operation = records.app_assistant_operation_set(AssistantOperationUpdate {
            operation_id: &operation_id,
            context,
            expected_revision: revision,
            status: "unknown",
            summary: message,
            result: &result,
            resource_refs: &resource_refs,
            permission_request: &permission_request,
            error: Some(message),
        })?;
    }
    Ok(public_operation(operation))
}

pub(super) fn public_operation(mut op: Value) -> Value {
    if let Some(fields) = op.as_object_mut() {
        fields.remove("_input");
        fields.remove("_operation_digest");
    }
    op
}

impl Shared {
    /// CAD-1700: the agent assistant entry. The master push revision is
    /// bumped AFTER the call, on success and failure alike, so a watcher
    /// that reads the new stamp always sees the rows the call wrote.
    pub(super) fn rpc_app_assistant_agent(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let out = self.rpc_app_assistant_agent_inner(method, params, peer_pid);
        if method == "app_assistant_invoke" {
            self.bump_master_rev();
        }
        out
    }

    fn rpc_app_assistant_agent_inner(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let allowed: &[&str] = match method {
            "app_assistant_actions" => &["install_id", "context_id", "message", "token"],
            "app_assistant_invoke" => &[
                "install_id",
                "context_id",
                "message",
                "token",
                "action_id",
                "operation_id",
                "input",
            ],
            "app_assistant_operation_show" => &[
                "install_id",
                "context_id",
                "message",
                "token",
                "operation_id",
            ],
            _ => return Err(Error::rejected("unknown assistant agent method")),
        };
        exact(params, allowed)?;
        let _assistant_guard = if matches!(
            method,
            "app_assistant_invoke" | "app_assistant_operation_show"
        ) {
            Some(
                self.app_assistant_lock
                    .lock()
                    .map_err(|_| Error::internal("assistant mutation lock is poisoned"))?,
            )
        } else {
            None
        };
        // The live-turn proof is re-read only after serialization is acquired.
        let scoped =
            self.scoped_chat_assistant(params, peer_pid, method, "app assistant action")?;
        let records = RecordStore::open(&self.state_dir, &scoped.install)?;
        if method == "app_assistant_operation_show" {
            let op = records.app_assistant_operation_get(
                required_str(params, "operation_id")?,
                &scoped.context,
            )?;
            // CAD-1636: a WhatsApp operation's stored output reads back only
            // in that conversation's own subject chat; a row without a
            // stored conversation id refuses.
            if op["action_id"]
                .as_str()
                .is_none_or(|action| action.starts_with("whatsapp."))
            {
                let conversation = op["_input"]["conversation_id"].as_str().ok_or_else(|| {
                    Error::rejected("WhatsApp assistant operation names no conversation")
                })?;
                scoped.require_whatsapp(conversation)?;
            }
            let op = normalize_interrupted_operation(&records, &scoped.context, op)?;
            return Ok(json!({"operation":op}));
        }
        // The consented descriptor is the allow-list (CAD-1518): only the
        // registered actions it declares are listed or invoked.
        let (descriptor_digest, descriptor) = install_descriptor(self, &scoped.install)?;
        if method == "app_assistant_actions" {
            let declared = descriptor
                .get("actions")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::rejected("assistant descriptor actions are invalid"))?;
            let mut actions = Vec::new();
            let full = self.store.crm_autonomy_is_full()?;
            for row in declared {
                let Some(id) = row.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(meta) = crate::app_assistant::registered_action(id) else {
                    continue;
                };
                let confirmation = if full && id == "customer.tags.update" {
                    "none"
                } else {
                    meta.confirmation
                };
                actions.push(json!({"id":id,"description":row["description"],"input_schema":meta.input_schema,"effect":meta.effect,"confirmation":confirmation,"availability":"available"}));
            }
            actions.extend(super::app_record_actions_rpc::declared_record_actions(
                self,
                &scoped.install,
            )?);
            return Ok(
                json!({"contract":crate::issue::app_assistant::CONTRACT,"actions":actions,"descriptor_digest":descriptor_digest}),
            );
        }
        let action_id = required_str(params, "action_id")?;
        let input = params
            .get("input")
            .filter(|v| v.is_object())
            .ok_or_else(|| Error::rejected("assistant action input must be an object"))?;
        // CAD-1415: a package-declared record action (not a host-registry id)
        // parks its own pending operation; the host registry is unchanged.
        if crate::app_assistant::registered_action(action_id).is_none() {
            let operation_id = required_str(params, "operation_id")?;
            return super::app_record_actions_rpc::invoke_record_action(
                self,
                &records,
                &scoped,
                operation_id,
                action_id,
                input,
            );
        }
        let meta = crate::app_assistant::registered_action(action_id)
            .ok_or_else(|| Error::rejected("unknown assistant action"))?;
        if !descriptor["actions"]
            .as_array()
            .is_some_and(|actions| actions.iter().any(|a| a["id"].as_str() == Some(action_id)))
        {
            return Err(Error::rejected(
                "action is not declared by this consented installation",
            ));
        }
        crate::app_assistant::validate_input(&meta.input_schema, input)?;
        if matches!(
            action_id,
            "campaigns.show" | "campaigns.create_draft" | "campaigns.approve" | "email.draft"
        ) {
            scoped.require_campaign(required_str(input, "campaign_id")?)?;
        }
        // CAD-1636: hosted only, and bound to this chat's subject, before
        // any operation row or draft exists.
        if matches!(
            action_id,
            "whatsapp.conversation.context" | "whatsapp.reply.draft"
        ) {
            self.whatsapp_door()?;
            scoped.require_whatsapp(required_str(input, "conversation_id")?)?;
        }
        let operation_id = required_str(params, "operation_id")?;
        let semantic = crate::store::app_runs::material_digest(
            &json!({"contract":crate::issue::app_assistant::CONTRACT,"action":action_id,"schema":meta.input_schema,"handler_version":meta.handler_version,"descriptor_digest":descriptor_digest,"input":input}),
        );
        let (mut op, is_new) = records.app_assistant_operation_create(
            operation_id,
            &scoped.context,
            action_id,
            &semantic,
            input,
        )?;
        if !is_new {
            let op = normalize_interrupted_operation(&records, &scoped.context, op)?;
            if action_id == "media.refine.prepare" && op["result"]["run_id"].as_str().is_some() {
                // Normalization strips `_input` for the wire. Resume reloads
                // the stored private record and keeps that same run.
                let private = records.app_assistant_operation_get(operation_id, &scoped.context)?;
                return self.refine_resume(
                    &records,
                    &scoped.install,
                    &scoped.context,
                    &scoped.message_id,
                    &private,
                );
            }
            return Ok(json!({"operation":op}));
        }
        if op.get("status").and_then(Value::as_str) != Some("running") {
            return Ok(json!({"operation":public_operation(op)}));
        }
        let execution = (|| -> Result<Value> {
            if action_id == "media.refine.prepare" {
                return self.refine_request_permission(
                    &records,
                    &scoped.install,
                    &scoped.context,
                    &scoped.message_id,
                    operation_id,
                    input,
                );
            }
            if is_paid_social(action_id) {
                return self.social_request_permission(
                    &records,
                    &scoped.install,
                    &scoped.context,
                    operation_id,
                    action_id,
                    input,
                );
            }
            if action_id == "customer.tags.update" {
                let customer = required_str(input, "customer_id")?;
                // Exact customer scope and current revision are checked before
                // presenting permission. No profile field except tags is stored.
                let old = records.app_record_show(&scoped.context, customer)?;
                let old_revision = old
                    .pointer("/record/revision")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| Error::rejected("customer record revision unavailable"))?;
                if input.get("expected_revision").and_then(Value::as_i64) != Some(old_revision) {
                    return Err(Error::rejected("customer record revision is stale"));
                }
                if records
                    .app_assistant_permission_find(&scoped.context, action_id, customer, "deny")?
                    .is_some_and(|p| p["state"].as_str() == Some("active"))
                {
                    return Err(Error::rejected("this action is blocked for this customer"));
                }
                if self.store.crm_autonomy_is_full()? {
                    let mut profile = crate::store::app_records::CustomerProfile::parse(
                        &old["record"]["profile"],
                    )
                    .map_err(|_| Error::rejected("customer profile cannot be safely updated"))?;
                    profile.tags = serde_json::from_value(input["tags"].clone())
                        .map_err(|_| Error::rejected("customer tags are invalid"))?;
                    let updated = records.app_record_update_as_actor(
                        &scoped.context,
                        customer,
                        old_revision,
                        &profile,
                        None,
                        &format!("agent:{}", scoped.caller),
                        None,
                    )?;
                    let op = records.app_assistant_operation_set(AssistantOperationUpdate {
                        operation_id,
                        context: &scoped.context,
                        expected_revision: 1,
                        status: "succeeded",
                        summary: "Updated tags on one customer",
                        result: &json!({
                            "customer_id": customer,
                            "tags": updated.pointer("/record/profile/tags").cloned().unwrap_or(json!([])),
                            "revision": updated.pointer("/record/revision"),
                        }),
                        resource_refs: &json!([{
                            "kind": "customer",
                            "id": customer,
                            "label": old.pointer("/record/profile/display_name").and_then(Value::as_str).unwrap_or("Customer"),
                        }]),
                        permission_request: &Value::Null,
                        error: None,
                    })?;
                    return Ok(json!({"operation": op}));
                }
                let mut refs = vec![
                    json!({"kind":"customer","id":customer,"label":old.pointer("/record/profile/display_name").and_then(Value::as_str).unwrap_or("Customer")}),
                ];
                let before_tags = old
                    .pointer("/record/profile/tags")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| Error::rejected("customer tags are unavailable"))?;
                let after_tags = input
                    .get("tags")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| Error::rejected("customer tags are invalid"))?;
                let preview = json!({"customer_label":old.pointer("/record/profile/display_name").and_then(Value::as_str).unwrap_or("Customer"),"before_tags":before_tags,"after_tags":after_tags,"expected_revision":old_revision});
                let grant_digest = crate::store::app_runs::material_digest(
                    &json!({"contract":crate::issue::app_assistant::CONTRACT,"action":action_id,"schema":meta.input_schema,"handler_version":meta.handler_version,"descriptor_digest":descriptor_digest}),
                );
                if records
                    .app_assistant_permission_find(&scoped.context, action_id, customer, "deny")?
                    .is_some_and(|p| p["state"].as_str() == Some("active"))
                {
                    op = records.app_assistant_operation_set(AssistantOperationUpdate {
                        operation_id,
                        context: &scoped.context,
                        expected_revision: 1,
                        status: "denied",
                        summary: "This action is blocked for this customer",
                        result: &Value::Null,
                        resource_refs: &Value::Array(vec![]),
                        permission_request: &Value::Null,
                        error: None,
                    })?;
                    return Ok(json!({"operation":op}));
                }
                if let Some(grant) = records
                    .app_assistant_permission_find(&scoped.context, action_id, customer, "allow")?
                    .filter(|p| p["state"].as_str() == Some("active"))
                {
                    if grant["semantics_digest"].as_str() != Some(grant_digest.as_str()) {
                        op = records.app_assistant_operation_set(AssistantOperationUpdate {
                            operation_id,
                            context: &scoped.context,
                            expected_revision: 1,
                            status: "failed",
                            summary: "Permission semantics changed; review and grant again",
                            result: &Value::Null,
                            resource_refs: &Value::Array(vec![]),
                            permission_request: &Value::Null,
                            error: Some("permission semantics changed"),
                        })?;
                        return Ok(json!({"operation":op}));
                    }
                    let mut profile = crate::store::app_records::CustomerProfile::parse(
                        &old["record"]["profile"],
                    )
                    .map_err(|_| Error::rejected("customer profile cannot be safely updated"))?;
                    profile.tags = serde_json::from_value(input["tags"].clone())
                        .map_err(|_| Error::rejected("customer tags are invalid"))?;
                    let updated = records.app_record_update(
                        &scoped.context,
                        customer,
                        old_revision,
                        &profile,
                        None,
                    )?;
                    op = records.app_assistant_operation_set(AssistantOperationUpdate {
                        operation_id,
                        context: &scoped.context,
                        expected_revision: 1,
                        status: "succeeded",
                        summary: "Updated tags on one customer",
                        result: &json!({"customer_id":customer,"tags":updated.pointer("/record/profile/tags").cloned().unwrap_or(json!([])),"revision":updated.pointer("/record/revision")}),
                        resource_refs: &json!([{"kind":"customer","id":customer,"label":preview["customer_label"]}]),
                        permission_request: &Value::Null,
                        error: None,
                    })?;
                    return Ok(json!({"operation":op}));
                }
                op = records.app_assistant_operation_set(AssistantOperationUpdate {
                    operation_id,
                    context: &scoped.context,
                    expected_revision: 1,
                    status: "pending_permission",
                    summary: "Review this tags-only customer change",
                    result: &json!({"preview":preview}),
                    resource_refs: &Value::Array(std::mem::take(&mut refs)),
                    permission_request: &json!({"reason":"Change tags on one customer","scope":{"install_id":scoped.install,"context_id":scoped.context,"action_id":action_id,"resource_id":customer},"allow_always":true,"preview":preview}),
                    error: None,
                })?;
                return Ok(json!({"operation":op}));
            }
            let result = match action_id {
                "customers.search" => records.app_record_list_paged(
                    &scoped.context,
                    input.get("query").and_then(Value::as_str),
                    input
                        .get("limit")
                        .and_then(Value::as_i64)
                        .unwrap_or(20)
                        .clamp(1, 20),
                    input.get("cursor").and_then(Value::as_str),
                ),
                "customers.show" => {
                    records.app_record_show(&scoped.context, required_str(input, "customer_id")?)
                }
                "segments.list" => records.app_segment_list(&scoped.context),
                "segments.show" => {
                    records.app_segment_show(&scoped.context, required_str(input, "segment_id")?)
                }
                "segments.preview" => {
                    records.app_segment_preview(&scoped.context, required_str(input, "segment_id")?)
                }
                "segments.save" => {
                    let mut segment_params = json!({
                        "install_id": &scoped.install,
                        "context_id": &scoped.context,
                        "segment_id": required_str(input, "segment_id")?,
                        "name": super::app_audiences_rpc::audience_name(input)?,
                        "predicates": input.get("predicates").cloned().unwrap_or(Value::Null),
                        "message": required_str(params, "message")?,
                        "token": required_str(params, "token")?,
                    });
                    if let Some(expected_revision) = input.get("expected_revision") {
                        segment_params["expected_revision"] = expected_revision.clone();
                    }
                    self.rpc_app_segment_assistant_save(&segment_params, peer_pid)
                }
                "segments.save_static" => {
                    let mut segment_params = json!({
                        "install_id": &scoped.install,
                        "context_id": &scoped.context,
                        "segment_id": required_str(input, "segment_id")?,
                        "name": super::app_audiences_rpc::audience_name(input)?,
                        "segment_type": "static",
                        "member_ids": input.get("member_ids").cloned().unwrap_or_else(|| json!([])),
                        "message": required_str(params, "message")?,
                        "token": required_str(params, "token")?,
                    });
                    if let Some(expected_revision) = input.get("expected_revision") {
                        segment_params["expected_revision"] = expected_revision.clone();
                    }
                    self.rpc_app_segment_assistant_save(&segment_params, peer_pid)
                }
                "segments.delete" => {
                    records.app_segment_delete(&scoped.context, required_str(input, "segment_id")?)
                }
                "social.drafts.status" => self.social_status(&records, &scoped.context),
                "campaigns.list" => records.app_content_list(&scoped.context),
                "campaigns.show" => {
                    records.app_content_show(&scoped.context, required_str(input, "campaign_id")?)
                }
                "campaigns.create_draft" => {
                    if let Some(segment) = input.get("segment_id").and_then(Value::as_str) {
                        records.app_segment_show(&scoped.context, segment)?;
                    }
                    let campaign = required_str(input, "campaign_id")?;
                    let name = required_str(input, "name")?;
                    let subject = "Campaign draft";
                    let block = json!({"type":"paragraph","text":"Draft content — review and edit before applying."});
                    let mut draft = crate::store::app_content::Draft::parse(subject, "", &[block])?;
                    draft.name = Some(name.to_string());
                    records.app_content_create_assistant(
                        &scoped.context,
                        campaign,
                        &scoped.caller,
                        operation_id,
                        &draft,
                        input.get("segment_id").and_then(Value::as_str),
                    )
                }
                "campaigns.apply" => {
                    let proposal = required_str(input, "proposal_id")?;
                    let shown = records.app_content_proposal_show(&scoped.context, proposal)?;
                    if let Some(campaign) = shown
                        .pointer("/proposal/campaign_id")
                        .and_then(Value::as_str)
                    {
                        scoped.require_campaign(campaign)?;
                    }
                    records.app_content_proposal_apply(
                        &scoped.context,
                        proposal,
                        input.get("expected_revision").and_then(Value::as_i64),
                    )
                }
                "campaigns.discard" => {
                    let proposal = required_str(input, "proposal_id")?;
                    let shown = records.app_content_proposal_show(&scoped.context, proposal)?;
                    if let Some(campaign) = shown
                        .pointer("/proposal/campaign_id")
                        .and_then(Value::as_str)
                    {
                        scoped.require_campaign(campaign)?;
                    }
                    records.app_content_proposal_discard(&scoped.context, proposal)
                }
                "campaigns.approve" => records.app_content_approve(
                    &scoped.context,
                    required_str(input, "campaign_id")?,
                    input
                        .get("expected_revision")
                        .and_then(Value::as_i64)
                        .ok_or_else(|| Error::rejected("expected content revision is required"))?,
                ),
                "audiences.prepare" => {
                    let base = AudienceBase::parse(
                        input
                            .get("base")
                            .ok_or_else(|| Error::rejected("audience base is required"))?,
                    )
                    .map_err(|_| Error::rejected("audience base exceeds its supported grammar"))?;
                    records.app_audience_prepare(
                        &scoped.context,
                        required_str(input, "freeze_id")?,
                        &base,
                        input.get("exclusion_list_id").and_then(Value::as_str),
                        input
                            .get("max_recipients")
                            .and_then(Value::as_i64)
                            .ok_or_else(|| {
                                Error::rejected("audience maximum recipients is required")
                            })?,
                    )
                }
                "audiences.show" => {
                    records.app_audience_show(&scoped.context, required_str(input, "freeze_id")?)
                }
                "sends.list" => {
                    let requested = input.get("campaign_id").and_then(Value::as_str);
                    if let Some(campaign) = requested {
                        scoped.require_campaign(campaign)?;
                    }
                    // A campaign-scoped conversation defaults to its own
                    // campaign; only a General chat may list the context.
                    let campaign = requested.or_else(|| {
                        scoped.subject.as_deref().and_then(|subject| {
                            subject.strip_prefix(crate::store::SUBJECT_CAMPAIGN)
                        })
                    });
                    let sends = records.app_campaign_sends(&scoped.context, campaign)?;
                    let mut out = Vec::with_capacity(sends.len());
                    for send in &sends {
                        let counts =
                            records.app_campaign_delivery_counts(&scoped.context, &send.send_id)?;
                        out.push(json!({
                            "send_id": send.send_id,
                            "campaign_id": send.campaign_id,
                            "state": send.state,
                            "send_digest": send.send_digest,
                            "created": send.created,
                            "approved_at": send.approved_at,
                            "counts": counts,
                            "delivery_claim": "smtp-acceptance-only",
                        }));
                    }
                    Ok(json!({"sends": out}))
                }
                "sends.show" => {
                    let send_id = required_str(input, "send_id")?;
                    let send = records
                        .app_campaign_send(&scoped.context, send_id)?
                        .ok_or_else(|| {
                            Error::rejected(
                                "campaign send is unavailable for this installation and context",
                            )
                        })?;
                    scoped.require_campaign(&send.campaign_id)?;
                    let deliveries = records.app_campaign_deliveries(&scoped.context, send_id)?;
                    let counts = records.app_campaign_delivery_counts(&scoped.context, send_id)?;
                    Ok(send_view(
                        &scoped.install,
                        &scoped.context,
                        &send,
                        &deliveries,
                        counts,
                    ))
                }
                "email.draft" => {
                    let draft_value = input
                        .get("draft")
                        .filter(|value| value.is_object())
                        .ok_or_else(|| Error::rejected("structured email draft is required"))?;
                    let draft_fields = draft_value.as_object().unwrap();
                    if draft_fields
                        .keys()
                        .any(|key| !matches!(key.as_str(), "subject" | "preheader" | "blocks"))
                    {
                        return Err(Error::rejected("email draft has unsupported fields"));
                    }
                    let subject = draft_value
                        .get("subject")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::rejected("email draft subject is required"))?;
                    let preheader = draft_value
                        .get("preheader")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let blocks = draft_value
                        .get("blocks")
                        .and_then(Value::as_array)
                        .ok_or_else(|| Error::rejected("email draft blocks are required"))?;
                    let draft =
                        crate::store::app_content::Draft::parse(subject, preheader, blocks)?;
                    records.app_content_assistant_draft(
                        &scoped.context,
                        required_str(input, "campaign_id")?,
                        required_str(input, "proposal_id")?,
                        &draft,
                        &scoped.caller,
                        &scoped.message_id,
                    )
                }
                "whatsapp.conversation.context" => self.whatsapp_copilot_context(
                    &scoped.install,
                    &scoped.context,
                    required_str(input, "conversation_id")?,
                ),
                "whatsapp.reply.draft" => {
                    self.whatsapp_copilot_draft(&scoped.install, &scoped.context, input)
                }
                _ => Err(Error::rejected("unknown assistant action")),
            }?;
            let resource_refs = resource_refs(action_id, &result);
            op = records.app_assistant_operation_set(AssistantOperationUpdate {
                operation_id,
                context: &scoped.context,
                expected_revision: 1,
                status: "succeeded",
                summary: "Action completed",
                result: &result,
                resource_refs: &resource_refs,
                permission_request: &Value::Null,
                error: None,
            })?;
            Ok(json!({"operation":op}))
        })();
        match execution {
            Ok(response) => Ok(response),
            Err(error) => {
                let definite_refusal = assistant_error_is_definite_refusal(&error);
                let (status, summary, detail) = if definite_refusal {
                    (
                        "failed",
                        "Action was refused",
                        "Action was refused without a recorded change",
                    )
                } else {
                    (
                        "unknown",
                        "Action outcome is uncertain; operator review required",
                        "Outcome may be uncertain; do not retry automatically",
                    )
                };
                let _ = records.app_assistant_operation_set(AssistantOperationUpdate {
                    operation_id,
                    context: &scoped.context,
                    expected_revision: 1,
                    status,
                    summary,
                    result: &Value::Null,
                    resource_refs: &Value::Array(vec![]),
                    permission_request: &Value::Null,
                    error: Some(detail),
                });
                Err(error)
            }
        }
    }

    /// CAD-1700: the operator assistant entry. The master push revision is
    /// bumped AFTER the call for the three write methods, on success and
    /// failure alike, so a watcher that reads the new stamp sees the rows
    /// the call wrote.
    pub(super) fn rpc_app_assistant_operator(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let out = self.rpc_app_assistant_operator_inner(method, params, peer_pid);
        if matches!(
            method,
            "app_assistant_decision"
                | "app_assistant_permission_revoke"
                | "app_assistant_permission_block"
        ) {
            self.bump_master_rev();
        }
        out
    }

    fn rpc_app_assistant_operator_inner(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let _assistant_guard = if matches!(
            method,
            "app_assistant_decision"
                | "app_assistant_permission_revoke"
                | "app_assistant_permission_block"
                | "app_assistant_operations"
                | "app_assistant_operation_operator_show"
        ) {
            Some(
                self.app_assistant_lock
                    .lock()
                    .map_err(|_| Error::internal("assistant mutation lock is poisoned"))?,
            )
        } else {
            None
        };
        self.operator_connection("app assistant operation", params, peer_pid)?;
        let allowed: &[&str] = match method {
            "app_assistant_actions_operator"
            | "app_assistant_operations"
            | "app_assistant_permissions" => &["install_id", "context_id"],
            "app_assistant_operation_operator_show" => {
                &["install_id", "context_id", "operation_id"]
            }
            "app_assistant_decision" => &[
                "install_id",
                "context_id",
                "operation_id",
                "decision",
                "expected_revision",
                "public_session",
            ],
            "app_assistant_permission_revoke" => &[
                "install_id",
                "context_id",
                "permission_id",
                "expected_revision",
            ],
            "app_assistant_permission_block" => {
                &["install_id", "context_id", "action_id", "resource_id"]
            }
            _ => return Err(Error::rejected("unknown assistant operator method")),
        };
        exact(params, allowed)?;
        let install = required_str(params, "install_id")?;
        let context = required_str(params, "context_id")?;
        self.store.app_context_proof(install, context)?;
        let records = RecordStore::open(&self.state_dir, install)?;
        match method {
            "app_assistant_actions_operator" => {
                let (digest, descriptor) = install_descriptor(self, install)?;
                let mut actions = Vec::new();
                for row in descriptor["actions"].as_array().into_iter().flatten() {
                    if let Some(id) = row["id"].as_str() {
                        if let Some(meta) = crate::app_assistant::registered_action(id) {
                            actions.push(json!({"id":id,"description":row["description"],"input_schema":meta.input_schema,"effect":meta.effect,"confirmation":meta.confirmation,"availability":"available"}));
                        }
                    }
                }
                actions.extend(super::app_record_actions_rpc::declared_record_actions(
                    self, install,
                )?);
                Ok(
                    json!({"contract":crate::issue::app_assistant::CONTRACT,"actions":actions,"descriptor_digest":digest}),
                )
            }
            "app_assistant_operation_operator_show" => {
                let operation = records
                    .app_assistant_operation_get(required_str(params, "operation_id")?, context)?;
                let operation = normalize_interrupted_operation(&records, context, operation)?;
                Ok(json!({"operation":operation}))
            }
            "app_assistant_operations" => {
                let mut operations = records.app_assistant_operations_list(context, 100)?;
                for operation in &mut operations {
                    *operation =
                        normalize_interrupted_operation(&records, context, operation.clone())?;
                }
                Ok(json!({"operations":operations}))
            }
            "app_assistant_permissions" => {
                Ok(json!({"permissions":records.app_assistant_permissions_list(context)?}))
            }
            "app_assistant_permission_revoke" => {
                let revision = params
                    .get("expected_revision")
                    .and_then(Value::as_i64)
                    .filter(|v| *v > 0)
                    .ok_or_else(|| {
                        Error::rejected("expected permission revision must be positive")
                    })?;
                Ok(
                    json!({"permission":records.app_assistant_permission_revoke(context, required_str(params,"permission_id")?, revision)?}),
                )
            }
            "app_assistant_permission_block" => {
                let action = required_str(params, "action_id")?;
                let resource = required_str(params, "resource_id")?;
                if action != "customer.tags.update" {
                    return Err(Error::rejected(
                        "only customer.tags.update supports a standing block",
                    ));
                }
                records.app_record_show(context, resource)?;
                let (descriptor_digest, descriptor) = install_descriptor(self, install)?;
                if !descriptor["actions"]
                    .as_array()
                    .is_some_and(|actions| actions.iter().any(|a| a["id"].as_str() == Some(action)))
                {
                    return Err(Error::rejected(
                        "action is not declared by this installation",
                    ));
                }
                let meta = crate::app_assistant::registered_action(action)
                    .ok_or_else(|| Error::rejected("unknown assistant action"))?;
                let digest = crate::store::app_runs::material_digest(
                    &json!({"contract":crate::issue::app_assistant::CONTRACT,"action":action,"schema":meta.input_schema,"handler_version":meta.handler_version,"descriptor_digest":descriptor_digest}),
                );
                let permission_id = uuid::Uuid::new_v4().to_string();
                let permission =
                    records.app_assistant_permission_save(AssistantPermissionWrite {
                        permission_id: &permission_id,
                        context,
                        action_id: action,
                        resource_id: resource,
                        effect: "deny",
                        semantics_digest: &digest,
                        scope_label: "This customer is blocked for tag updates",
                    })?;
                Ok(json!({"permission":permission}))
            }
            "app_assistant_decision" => {
                let human = decision_human(self, params, peer_pid)?;
                self.decide_assistant_operation(&records, install, context, params, human.as_ref())
            }
            _ => Err(Error::rejected("unknown assistant operator method")),
        }
    }

    fn decide_assistant_operation(
        self: &Arc<Self>,
        records: &RecordStore,
        install: &str,
        context: &str,
        params: &Value,
        human: Option<&Value>,
    ) -> Result<Value> {
        let operation_id = required_str(params, "operation_id")?;
        let decision = required_str(params, "decision")?;
        if !matches!(decision, "allow_once" | "allow_always" | "deny") {
            return Err(Error::rejected(
                "assistant decision must be allow_once, allow_always or deny",
            ));
        }
        let expected = params
            .get("expected_revision")
            .and_then(Value::as_i64)
            .filter(|v| *v > 0)
            .ok_or_else(|| Error::rejected("expected operation revision must be positive"))?;
        let op = records.app_assistant_operation_get(operation_id, context)?;
        if op["status"].as_str() != Some("pending_permission")
            || op["revision"].as_i64() != Some(expected)
        {
            return Err(Error::rejected(
                "assistant operation is not pending at the expected revision",
            ));
        }
        if decision == "deny" {
            let result = records.app_assistant_operation_set(AssistantOperationUpdate {
                operation_id,
                context,
                expected_revision: expected,
                status: "denied",
                summary: if op["action_id"] == "media.refine.prepare" {
                    "Denied; no image edit was started"
                } else {
                    "Denied; no customer change was made"
                },
                result: &Value::Null,
                resource_refs: &Value::Array(vec![]),
                permission_request: &Value::Null,
                error: None,
            })?;
            return Ok(json!({"operation":result}));
        }
        let action = required_str(&op, "action_id")?;
        // CAD-1415: a declared record action executes only as allow-once.
        if crate::app_assistant::registered_action(action).is_none() {
            if decision != "allow_once" {
                return Err(Error::rejected("record actions are allowed one time only"));
            }
            return super::app_record_actions_rpc::decide_record_action(
                self,
                records,
                install,
                context,
                &op,
                operation_id,
                expected,
            );
        }
        if action != "customer.tags.update"
            && action != "media.refine.prepare"
            && !is_paid_social(action)
        {
            return Err(Error::rejected(
                "no operator decision handler is registered for this action",
            ));
        }
        if is_paid_social(action) && decision != "allow_once" {
            return Err(Error::rejected(
                "paid social actions are allowed one time only",
            ));
        }
        let (descriptor_digest, descriptor) = install_descriptor(self, install)?;
        if !descriptor["actions"]
            .as_array()
            .is_some_and(|actions| actions.iter().any(|a| a["id"].as_str() == Some(action)))
        {
            return Err(Error::rejected(
                "action is no longer declared by this installation",
            ));
        }
        let meta = crate::app_assistant::registered_action(action)
            .ok_or_else(|| Error::rejected("unknown assistant action"))?;
        let input = op
            .get("_input")
            .cloned()
            .ok_or_else(|| Error::rejected("assistant operation input is unavailable"))?;
        crate::app_assistant::validate_input(&meta.input_schema, &input)?;
        let operation_digest = crate::store::app_runs::material_digest(
            &json!({"contract":crate::issue::app_assistant::CONTRACT,"action":action,"schema":meta.input_schema,"handler_version":meta.handler_version,"descriptor_digest":descriptor_digest,"input":input}),
        );
        if op.get("_operation_digest").and_then(Value::as_str) != Some(operation_digest.as_str()) {
            return Err(Error::rejected(
                "assistant action semantics changed; permission must be requested again",
            ));
        }
        if action == "media.refine.prepare" {
            if decision != "allow_once" {
                return Err(Error::rejected(
                    "a library image refinement is allowed one time only",
                ));
            }
            return self.refine_run(
                records,
                install,
                context,
                &op,
                human,
                params.get("public_session").and_then(Value::as_str),
            );
        }
        if is_paid_social(action) {
            return self.social_run(records, install, context, &op, &input);
        }
        let customer = required_str(&input, "customer_id")?;
        let preview = op
            .pointer("/permission_request/preview")
            .ok_or_else(|| Error::rejected("assistant permission preview is unavailable"))?;
        let current = records.app_record_show(context, customer)?;
        let revision = current
            .pointer("/record/revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::rejected("customer record revision unavailable"))?;
        if Some(revision) != preview.get("expected_revision").and_then(Value::as_i64)
            || current.pointer("/record/profile/tags") != preview.get("before_tags")
            || input.get("tags") != preview.get("after_tags")
        {
            return Err(Error::rejected(
                "customer changed since permission preview; review the updated impact",
            ));
        }
        let grant_digest = crate::store::app_runs::material_digest(
            &json!({"contract":crate::issue::app_assistant::CONTRACT,"action":action,"schema":meta.input_schema,"handler_version":meta.handler_version,"descriptor_digest":descriptor_digest}),
        );
        let existing = records.app_assistant_permission_find(context, action, customer, "deny")?;
        if existing
            .as_ref()
            .is_some_and(|p| p["state"].as_str() == Some("active"))
        {
            return Err(Error::rejected("this action is blocked for this customer"));
        }
        let mut profile =
            crate::store::app_records::CustomerProfile::parse(&current["record"]["profile"])
                .map_err(|_| Error::rejected("customer profile cannot be safely updated"))?;
        profile.tags = serde_json::from_value(input["tags"].clone())
            .map_err(|_| Error::rejected("customer tags are invalid"))?;
        let updated = records.app_record_update(context, customer, revision, &profile, None)?;
        if decision == "allow_always" {
            let permission_id = uuid::Uuid::new_v4().to_string();
            let scope_label = format!(
                "{} — this installation, this customer",
                preview["customer_label"].as_str().unwrap_or("Customer")
            );
            records.app_assistant_permission_save(AssistantPermissionWrite {
                permission_id: &permission_id,
                context,
                action_id: action,
                resource_id: customer,
                effect: "allow",
                semantics_digest: &grant_digest,
                scope_label: &scope_label,
            })?;
        }
        let result = records.app_assistant_operation_set(AssistantOperationUpdate {
            operation_id,
            context,
            expected_revision: expected,
            status: "succeeded",
            summary: "Updated tags on one customer",
            result: &json!({"customer_id":customer,"tags":updated.pointer("/record/profile/tags").cloned().unwrap_or(json!([])),"revision":updated.pointer("/record/revision")}),
            resource_refs: &json!([{"kind":"customer","id":customer,"label":preview["customer_label"]}]),
            permission_request: &Value::Null,
            error: None,
        })?;
        Ok(json!({"operation":result}))
    }

    /// Park a reference-edit quote. No provider submit happens here.
    fn refine_request_permission(
        &self,
        records: &RecordStore,
        install: &str,
        context: &str,
        message_id: &str,
        operation_id: &str,
        input: &Value,
    ) -> Result<Value> {
        let (asset_id, revision, sha256, instructions) = refine_input(input)?;
        self.refine_turn_pin(message_id, asset_id, revision, sha256)?;
        let (original, title) = self.refine_source_current(asset_id, revision, sha256)?;
        let (slot, quote) = self.refine_quote(install, context, asset_id, revision, sha256)?;
        let micros = quote["total_price_micros"]
            .as_u64()
            .ok_or_else(|| Error::rejected("reference-edit quote is missing its exact price"))?;
        let model = crate::platform::agenticos_external::reviewed_image_model();
        let reason = "Review one library image reference edit. The card freezes the source pin, instructions, model, exact price, and maximum spend. Charged once. The source stays unchanged.";
        records
            .app_assistant_operation_set(crate::store::app_records::AssistantOperationUpdate {
                operation_id,
                context,
                expected_revision: 1,
                status: "pending_permission",
                summary: "Review this reference edit before it costs anything",
                result: &Value::Null,
                resource_refs: &json!([{"kind":"media","id":asset_id,"label":"Library image"}]),
                permission_request: &json!({
                    "reason": reason,
                    "scope": {"install_id":install,"context_id":context,"action_id":"media.refine.prepare","resource_id":asset_id},
                    "allow_always": false,
                    "preview_kind": "media_refine",
                    "preview": {
                        "asset_id": asset_id,
                        "revision": revision,
                        "sha256": sha256,
                        "title": title,
                        "artifact_ref": original.reference,
                        "instructions": instructions,
                        "workflow": "refine-image",
                        "slot": slot,
                        "model": model,
                        "quote": quote,
                        "estimate_micros": micros,
                        "currency": "USD",
                        "ceiling_micros": micros,
                        "allow_once": true,
                    },
                }),
                error: None,
            })
            .map(|op| json!({"operation": public_operation(op)}))
    }

    fn refine_source_current(
        &self,
        asset_id: &str,
        revision: i64,
        sha256: &str,
    ) -> Result<(crate::store::app_media::MediaOriginalProof, String)> {
        let image = self.store.app_media_context_image(asset_id)?;
        let original = image.original;
        let hex = sha256
            .strip_prefix("sha256:")
            .filter(|hex| hex.len() == 64)
            .ok_or_else(|| Error::rejected("library image digest is not canonical"))?;
        let reference_ok = original
            .reference
            .split_once('.')
            .is_some_and(|(job, digest)| {
                digest == hex && job.starts_with("med_") && !original.reference.contains("://")
            });
        if image.asset.revision != revision || original.sha256 != sha256 || !reference_ok {
            return Err(Error::rejected(
                "producer reference is not the retained original artifact",
            ));
        }
        self.with_media_source_current(&original, || Ok(()))?;
        Ok((original, image.asset.title))
    }

    fn refine_quote(
        &self,
        install: &str,
        context: &str,
        asset_id: &str,
        revision: i64,
        sha256: &str,
    ) -> Result<(String, Value)> {
        let _ = (asset_id, revision, sha256);
        let pm = self.pm_at(&self.pm_dir()?)?;
        let (slot, request) = workspace::with_runtime_snapshot(&pm, install, |row, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let manifest = crate::issue::app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?;
            let text = files
                .get("workflows/refine-image.md")
                .ok_or_else(|| Error::rejected("this installation has no refine-image workflow"))?;
            let template = crate::issue::workflow::parse_template(text)
                .map_err(|_| Error::rejected("refine-image declarations refused"))?;
            if template.execution != crate::issue::workflow::Execution::Host {
                return Err(Error::rejected("refine-image must run on the host"));
            }
            let expected = [
                "library_asset_id",
                "library_revision",
                "library_sha256",
                "instructions",
            ];
            let inputs_ok = template.input_order.len() == expected.len()
                && template.inputs.len() == expected.len()
                && template
                    .input_order
                    .iter()
                    .zip(expected)
                    .all(|(got, want)| got == want)
                && expected.iter().all(|name| {
                    template.inputs.get(*name).is_some_and(|spec| {
                        !spec.optional
                            && spec.kind.is_none()
                            && spec.default.is_none()
                            && !spec.context_default
                    })
                });
            if !inputs_ok {
                return Err(Error::rejected(
                    "refine-image inputs must be the four required strings library_asset_id, library_revision, library_sha256, instructions",
                ));
            }
            let slots: Vec<&str> = template
                .capability_slots
                .iter()
                .filter(|slot| {
                    manifest.capabilities.get(*slot).is_some_and(|need| {
                        need.capability == "media.generate" && need.effect == "draft"
                    })
                })
                .map(String::as_str)
                .collect();
            let [slot] = slots.as_slice() else {
                return Err(Error::rejected(
                    "refine-image must declare one media.generate draft slot",
                ));
            };
            let proof = self
                .app_binding_live(install, Some(context), slot, row, files)?
                .ok_or_else(|| Error::rejected("capability binding is absent — finish setup"))?;
            Ok((
                (*slot).to_owned(),
                self.prepare_app_capability_quote(&proof)?.reference_edit(),
            ))
        })?;
        let quote = request.fetch()?;
        if !quote.valid() || quote.currency != "USD" || quote.units != 1 {
            return Err(Error::rejected(
                "reference-edit quote is outside the reviewed price bound",
            ));
        }
        Ok((slot, json!(quote)))
    }

    fn refine_run(
        self: &Arc<Self>,
        records: &RecordStore,
        install: &str,
        context: &str,
        op: &Value,
        human: Option<&Value>,
        public_session: Option<&str>,
    ) -> Result<Value> {
        let operation_id = required_str(op, "id")?;
        let input = op
            .get("_input")
            .ok_or_else(|| Error::rejected("assistant operation input is unavailable"))?;
        let expected = op["revision"]
            .as_i64()
            .ok_or_else(|| Error::rejected("assistant operation revision is unavailable"))?;
        let preview = op
            .pointer("/permission_request/preview")
            .ok_or_else(|| Error::rejected("assistant permission preview is unavailable"))?;
        let (asset_id, revision, sha256, instructions) = refine_input(input)?;
        if op.pointer("/permission_request/preview_kind") != Some(&json!("media_refine"))
            || op.pointer("/permission_request/allow_always") != Some(&json!(false))
            || preview["asset_id"] != asset_id
            || preview["revision"] != revision
            || preview["sha256"] != sha256
            || preview["instructions"] != instructions
            || preview["workflow"] != "refine-image"
            || preview["model"] != crate::platform::agenticos_external::reviewed_image_model()
            || preview["currency"] != "USD"
            || preview["allow_once"] != true
        {
            return Err(Error::rejected(
                "the library image changed since the price was shown",
            ));
        }
        let (original, title) = self.refine_source_current(asset_id, revision, sha256)?;
        if preview["title"] != title || preview["artifact_ref"] != original.reference {
            return Err(Error::rejected(
                "producer reference changed since the price was shown",
            ));
        }
        let (slot, live) = self.refine_quote(install, context, asset_id, revision, sha256)?;
        let confirmed = preview["ceiling_micros"]
            .as_u64()
            .ok_or_else(|| Error::rejected("the confirmed reference-edit ceiling is missing"))?;
        let live_micros = live["total_price_micros"].as_u64().unwrap_or(u64::MAX);
        if slot != preview["slot"].as_str().unwrap_or("") || live_micros > confirmed {
            return Err(Error::rejected(
                "price_changed: the live reference-edit price is above the confirmed price",
            ));
        }
        if live != preview["quote"] || preview["estimate_micros"] != live["total_price_micros"] {
            return Err(Error::rejected(
                "price_changed: the confirmed reference-edit quote is no longer current",
            ));
        }
        let mut params = json!({
            "install_id": install,
            "context_id": context,
            "workflow": "refine-image",
            "request_id": operation_id,
            "inputs": {
                "library_asset_id": asset_id,
                "library_revision": revision.to_string(),
                "library_sha256": sha256,
                "instructions": instructions,
            }
        });
        // Reuse the verified public approver's native session for creator
        // attribution and saved-account ownership, not the local operator fallback.
        if let Some(token) = public_session {
            params["token"] = json!(token);
            params["origin"] = json!("public");
        }
        let expected_quote = json!({slot: live});
        let created = self.create_app_run_as(&params, None, Some(&expected_quote), human)?;
        let run_id = created["id"]
            .as_str()
            .ok_or_else(|| Error::rejected("image run id is missing"))?
            .to_owned();
        let digest = created["snapshot_digest"]
            .as_str()
            .ok_or_else(|| Error::rejected("image run digest is missing"))?
            .to_owned();
        if created["state"] == "awaiting_approval" {
            self.with_app_run_current(&run_id, |bundle| {
                self.store
                    .app_run_decide_as(&run_id, Some(&digest), false, Some(bundle), human)
            })?;
        }
        let pinned = json!({"run_id": run_id, "source_asset_id": asset_id});
        records.app_assistant_operation_set(
            crate::store::app_records::AssistantOperationUpdate {
                operation_id,
                context,
                expected_revision: expected,
                status: "running",
                summary: "Submitting the reference edit",
                result: &pinned,
                resource_refs: &json!([{"kind":"media","id":asset_id,"label":"Library image"}]),
                permission_request: &Value::Null,
                error: None,
            },
        )?;
        let outcome = self.refine_dispatch(&run_id, human);
        self.refine_finish(records, context, op, expected + 1, &run_id, outcome)
    }

    fn refine_resume(
        self: &Arc<Self>,
        records: &RecordStore,
        _install: &str,
        context: &str,
        message_id: &str,
        op: &Value,
    ) -> Result<Value> {
        let run_id = op["result"]["run_id"]
            .as_str()
            .ok_or_else(|| Error::rejected("reference edit run is not pinned"))?
            .to_owned();
        let input = op
            .get("_input")
            .ok_or_else(|| Error::rejected("assistant operation input is unavailable"))?;
        let (asset_id, revision, sha256, _) = refine_input(input)?;
        self.refine_turn_pin(message_id, asset_id, revision, sha256)?;
        self.refine_source_current(asset_id, revision, sha256)?;
        let expected = op["revision"]
            .as_i64()
            .ok_or_else(|| Error::rejected("assistant operation revision is unavailable"))?;
        // Acting human stays unset: dispatch re-reads the sealed approver.
        let outcome = self.refine_dispatch(&run_id, None);
        self.refine_finish(records, context, op, expected, &run_id, outcome)
    }

    fn refine_dispatch(&self, run_id: &str, acting: Option<&Value>) -> Result<Value> {
        let shown = self.store.app_run_show(run_id)?;
        if shown["state"] == "succeeded" {
            return Ok(shown);
        }
        self.dispatch_app_run_as(run_id, acting)
    }

    /// The invoking message's one durable library pin. Caller image fields
    /// are not authority; they must equal this stored selector.
    fn refine_turn_pin(
        &self,
        message_id: &str,
        asset_id: &str,
        revision: i64,
        sha256: &str,
    ) -> Result<()> {
        let pinned = self.store.message_media_refs(message_id)?;
        let rows = pinned
            .as_ref()
            .and_then(Value::as_array)
            .filter(|rows| rows.len() == 1)
            .ok_or_else(|| {
                Error::rejected(
                    "refinement needs exactly one library image on the invoking message",
                )
            })?;
        let row = rows[0].as_object().filter(|row| {
            row.len() == 3
                && row.contains_key("asset_id")
                && row.contains_key("revision")
                && row.contains_key("sha256")
        });
        let Some(row) = row else {
            return Err(Error::rejected(
                "refinement image pin is not a durable library reference",
            ));
        };
        if row["asset_id"].as_str() != Some(asset_id)
            || row["revision"].as_i64() != Some(revision)
            || row["sha256"].as_str() != Some(sha256)
        {
            return Err(Error::rejected(
                "refinement source is not the library image pinned to this turn",
            ));
        }
        Ok(())
    }

    fn refine_finish(
        &self,
        records: &RecordStore,
        context: &str,
        op: &Value,
        expected: i64,
        run_id: &str,
        outcome: Result<Value>,
    ) -> Result<Value> {
        use crate::store::app_records::AssistantOperationUpdate as Update;
        let operation_id = required_str(op, "id")?;
        let input = op
            .get("_input")
            .ok_or_else(|| Error::rejected("assistant operation input is unavailable"))?;
        let asset_id = required_str(input, "asset_id")?;
        let sha256 = required_str(input, "sha256")?;
        let failure = outcome.as_ref().err().map(ToString::to_string);
        let (status, summary, result, error) = match outcome {
            Ok(run) => {
                let linked = self.store.app_media_lineage_for_run(run_id)?;
                let matched = linked.as_ref().is_some_and(|row| {
                    row["source_asset_id"] == asset_id && row["source_sha256"] == sha256
                });
                let body = json!({
                    "run_id": run_id,
                    "run_state": run["state"],
                    "source_asset_id": asset_id,
                    "result_asset_id": linked.as_ref().and_then(|row| row["asset_id"].as_str()),
                    "result_revision": linked.as_ref().and_then(|row| row["revision"].as_i64()),
                    "lineage": linked,
                });
                if run["state"] == "succeeded" && matched {
                    (
                        "succeeded",
                        "Retained a new image linked to the source".to_owned(),
                        body,
                        None,
                    )
                } else {
                    (
                        "unknown",
                        "The reference edit did not retain this run's linked image".to_owned(),
                        body,
                        Some("linked catalog image was not recorded for this run"),
                    )
                }
            }
            Err(error) => {
                let unknown = error.kind() != "rejected";
                (
                    if unknown { "unknown" } else { "failed" },
                    if unknown {
                        "Reference edit outcome is uncertain; retry reuses the same run".to_owned()
                    } else {
                        "Reference edit was refused".to_owned()
                    },
                    json!({"run_id": run_id, "source_asset_id": asset_id}),
                    failure.as_deref(),
                )
            }
        };
        let done = records.app_assistant_operation_set(Update {
            operation_id,
            context,
            expected_revision: expected,
            status,
            summary: &summary,
            result: &result,
            resource_refs: &json!([{"kind":"media","id":asset_id,"label":"Library image"}]),
            permission_request: &Value::Null,
            error,
        })?;
        Ok(json!({"operation": public_operation(done)}))
    }
}

/// Canonical owner for a decision. Absent `public_session` is the
/// credential-less local operator. A present proof is re-read from the
/// daemon's own session row; frame human fields are not accepted.
fn decision_human(shared: &Shared, params: &Value, peer_pid: u32) -> Result<Option<Value>> {
    match params.get("public_session") {
        None => Ok(None),
        Some(_) => {
            if shared.slot_identity(peer_pid)?.is_some() {
                return Err(Error::rejected(
                    "agent connections cannot carry a board owner's proof",
                ));
            }
            Ok(shared.verified_owner_human(params)?.map(|human| {
                json!({
                    "sub": human["sub"],
                    "company": human["company"],
                    "session": human["session"],
                    "role": human["role"],
                    "display": human["display"],
                })
            }))
        }
    }
}

fn refine_input(input: &Value) -> Result<(&str, i64, &str, &str)> {
    let asset_id = required_str(input, "asset_id")?;
    let revision = input["revision"]
        .as_i64()
        .filter(|revision| *revision >= 1)
        .ok_or_else(|| Error::rejected("library image revision must be positive"))?;
    let sha256 = required_str(input, "sha256")?;
    let hex = sha256.strip_prefix("sha256:").unwrap_or("");
    if sha256.len() != 71
        || hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::rejected("library image digest is not canonical"));
    }
    let instructions = required_str(input, "instructions")?;
    if instructions.trim().is_empty() {
        return Err(Error::rejected("reference instructions are blank"));
    }
    Ok((asset_id, revision, sha256, instructions))
}

fn resource_refs(action: &str, result: &Value) -> Value {
    if action == "social.drafts.status" {
        return Value::Array(
            result["drafts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| {
                    let id = row["draft_id"].as_str()?;
                    let caption = row["caption"].as_str().unwrap_or("").trim();
                    let label = if caption.is_empty() { "Draft" } else { caption };
                    Some(json!({"kind":"social_draft","id":id,"label":label}))
                })
                .collect(),
        );
    }
    if action == "customers.search" {
        return Value::Array(
            result
                .get("records")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| {
                    let record = row.get("record").unwrap_or(row);
                    let id = record.get("id")?.as_str()?;
                    let label = record
                        .pointer("/profile/display_name")
                        .and_then(Value::as_str)
                        .unwrap_or("Customer");
                    Some(json!({"kind":"customer","id":id,"label":label}))
                })
                .take(20)
                .collect(),
        );
    }
    if action == "segments.preview" {
        return result
            .pointer("/base/segment_id")
            .and_then(Value::as_str)
            .map(|id| json!([{"kind":"segment","id":id,"label":"Segment audience preview"}]))
            .unwrap_or_else(|| json!([]));
    }
    let (kind, id, label) = match action {
        "customers.show" => (
            "customer",
            result.pointer("/record/id").and_then(Value::as_str),
            result
                .pointer("/record/profile/display_name")
                .and_then(Value::as_str),
        ),
        "segments.show" | "segments.save" | "segments.save_static" => (
            "segment",
            result.pointer("/segment/id").and_then(Value::as_str),
            result.pointer("/segment/name").and_then(Value::as_str),
        ),
        "campaigns.show" | "campaigns.create_draft" | "campaigns.approve" => (
            "campaign",
            result
                .pointer("/content/campaign_id")
                .and_then(Value::as_str),
            result.pointer("/content/name").and_then(Value::as_str),
        ),
        "campaigns.apply" => (
            "campaign",
            result
                .pointer("/content/campaign_id")
                .and_then(Value::as_str),
            Some("Campaign content"),
        ),
        "sends.show" => (
            "send",
            result.pointer("/send/send_id").and_then(Value::as_str),
            Some("Campaign send"),
        ),
        "email.draft" => (
            "campaign",
            result
                .pointer("/proposal/campaign_id")
                .and_then(Value::as_str),
            Some("Email proposal"),
        ),
        _ => ("", None, None),
    };
    id.map(|id| json!([{"kind":kind,"id":id,"label":label.unwrap_or(kind)}]))
        .unwrap_or_else(|| json!([]))
}
