//! CAD-1184 generic CRM assistant RPCs. Agent calls use only the existing
//! live-turn verifier; operator decision calls prove the operator connection.
use serde_json::{json, Value};
use std::sync::Arc;

use super::{required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::app_catalog::workspace;
use crate::store::app_records::{AssistantOperationUpdate, AssistantPermissionWrite, RecordStore};

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

fn install_descriptor(shared: &Shared, install: &str) -> Result<(String, String, Value)> {
    let pm = shared.pm_at(&shared.pm_dir()?)?;
    workspace::with_runtime_read(&pm, install, |row, files| {
        let digest = row
            .get("digest")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("installation digest unavailable"))?
            .to_string();
        let state = shared.store.app_capability_status(install, &digest)?;
        if state.get("state").and_then(Value::as_str) != Some("approved") {
            return Err(Error::rejected("app assistant descriptor is not consented"));
        }
        let text = files
            .get(crate::issue::app_assistant::FILE)
            .ok_or_else(|| Error::rejected("installation has no assistant actions"))?;
        let descriptor = crate::issue::app_assistant::size_and_json(text)?;
        Ok((
            digest,
            descriptor
                .get("app")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            descriptor,
        ))
    })
}

fn assistant_error_is_definite_refusal(error: &Error) -> bool {
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

fn public_operation(mut op: Value) -> Value {
    if let Some(fields) = op.as_object_mut() {
        fields.remove("_input");
        fields.remove("_operation_digest");
    }
    op
}

impl Shared {
    pub(super) fn rpc_app_assistant_agent(
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
        let scoped = self.scoped_chat_assistant(params, peer_pid, "app assistant action")?;
        let records = RecordStore::open(&self.state_dir, &scoped.install)?;
        if method == "app_assistant_operation_show" {
            let op = records.app_assistant_operation_get(
                required_str(params, "operation_id")?,
                &scoped.context,
            )?;
            let op = normalize_interrupted_operation(&records, &scoped.context, op)?;
            return Ok(json!({"operation":op}));
        }
        let (descriptor_digest, _app, descriptor) = install_descriptor(self, &scoped.install)?;
        if method == "app_assistant_actions" {
            let declared = descriptor
                .get("actions")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::rejected("assistant descriptor actions are invalid"))?;
            let mut actions = Vec::new();
            for row in declared {
                let Some(id) = row.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(meta) = crate::app_assistant::registered_action(id) else {
                    continue;
                };
                actions.push(json!({"id":id,"description":row["description"],"input_schema":meta.input_schema,"effect":meta.effect,"confirmation":meta.confirmation,"availability":"available"}));
            }
            return Ok(
                json!({"contract":crate::issue::app_assistant::CONTRACT,"actions":actions,"descriptor_digest":descriptor_digest}),
            );
        }
        let action_id = required_str(params, "action_id")?;
        let input = params
            .get("input")
            .filter(|v| v.is_object())
            .ok_or_else(|| Error::rejected("assistant action input must be an object"))?;
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
            "campaigns.show" | "campaigns.create_draft" | "email.draft"
        ) {
            scoped.require_campaign(required_str(input, "campaign_id")?)?;
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
            return Ok(json!({"operation":op}));
        }
        if op.get("status").and_then(Value::as_str) != Some("running") {
            return Ok(json!({"operation":public_operation(op)}));
        }
        let execution = (|| -> Result<Value> {
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

    pub(super) fn rpc_app_assistant_operator(
        &self,
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
                let (digest, _, descriptor) = install_descriptor(self, install)?;
                let mut actions = Vec::new();
                for row in descriptor["actions"].as_array().into_iter().flatten() {
                    if let Some(id) = row["id"].as_str() {
                        if let Some(meta) = crate::app_assistant::registered_action(id) {
                            actions.push(json!({"id":id,"description":row["description"],"input_schema":meta.input_schema,"effect":meta.effect,"confirmation":meta.confirmation,"availability":"available"}));
                        }
                    }
                }
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
                let (descriptor_digest, _, descriptor) = install_descriptor(self, install)?;
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
                self.decide_assistant_operation(&records, install, context, params)
            }
            _ => Err(Error::rejected("unknown assistant operator method")),
        }
    }

    fn decide_assistant_operation(
        &self,
        records: &RecordStore,
        install: &str,
        context: &str,
        params: &Value,
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
                summary: "Denied; no customer change was made",
                result: &Value::Null,
                resource_refs: &Value::Array(vec![]),
                permission_request: &Value::Null,
                error: None,
            })?;
            return Ok(json!({"operation":result}));
        }
        let action = required_str(&op, "action_id")?;
        if action != "customer.tags.update" {
            return Err(Error::rejected(
                "no operator decision handler is registered for this action",
            ));
        }
        let (descriptor_digest, _, descriptor) = install_descriptor(self, install)?;
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
}

fn resource_refs(action: &str, result: &Value) -> Value {
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
        "segments.show" | "segments.save" => (
            "segment",
            result.pointer("/segment/id").and_then(Value::as_str),
            result.pointer("/segment/name").and_then(Value::as_str),
        ),
        "campaigns.show" | "campaigns.create_draft" => (
            "campaign",
            result
                .pointer("/content/campaign_id")
                .and_then(Value::as_str),
            result.pointer("/content/name").and_then(Value::as_str),
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
