//! CAD-1415: generic declaration-driven `record.create` / `record.update`.
//!
//! An installed package declares its record actions in `app-actions.json`
//! (see `crate::issue::app_action`). This module is the one host path that
//! turns such a declaration into a proposal and, on a native operator
//! decision, into a store mutation. It holds no package-specific names: the
//! action id, label and fields all come from the approved bundle.
//!
//! Authority split (mirrors `customer.tags.update`). A chat invocation only
//! validates the input and parks a `pending_permission` operation with a
//! preview. Only `decide_record_action` mutates, and only after the operator
//! proof, a re-derived operation digest, the same approved declaration, and
//! the expected record revision. Create refuses an existing id; update merges
//! the declared fields into the stored profile, preserving consent and every
//! unpatched field. No standing permission exists: `allow_once` only.
use serde_json::{json, Map, Value};

use super::app_content_rpc::ScopedChat;
use super::Shared;
use crate::error::{Error, Result};
use crate::issue::app_action::{Action, Field, Operation};
use crate::issue::app_catalog::workspace;
use crate::store::app_records::{AssistantOperationUpdate, CustomerProfile, RecordStore};

/// Host-owned keys every record invocation carries. They are never declared
/// fields: the target and the compare-and-set revision are host semantics.
const RECORD_ID: &str = "record_id";
const EXPECTED_REVISION: &str = "expected_revision";
/// Per-element bound advertised for `tags` (Unicode characters).
const TAG_MAX_CHARS: usize = 40;

/// The declared action an invocation names, when the approved bundle carries
/// it. `None` means the id is not a record declaration at all.
fn declared_action(
    descriptor: &crate::issue::app_action::Descriptor,
    action_id: &str,
) -> Option<Action> {
    descriptor
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .cloned()
}

fn operation_name(action: &Action) -> &'static str {
    match action.operation {
        Operation::RecordCreate => "record.create",
        Operation::RecordUpdate => "record.update",
    }
}

/// Input schema for one declared action, as discovery shows it. Host keys are
/// always present; the declared fields follow. Update requires the revision.
fn input_schema(action: &Action) -> Value {
    let mut properties = Map::new();
    let mut required = vec![json!(RECORD_ID)];
    properties.insert(
        RECORD_ID.into(),
        json!({"type":"string","minLength":1,"maxLength":64}),
    );
    if action.operation == Operation::RecordUpdate {
        properties.insert(
            EXPECTED_REVISION.into(),
            json!({"type":"integer","minimum":1}),
        );
        required.push(json!(EXPECTED_REVISION));
    }
    // An update is a partial patch: declared `required` applies to create only.
    for field in &action.input.fields {
        let rule = match field.field_type.as_str() {
            "tags" => {
                json!({"type":"array","maxItems":field.max_items.unwrap_or(16),"items":{"type":"string","minLength":1,"maxLength":TAG_MAX_CHARS}})
            }
            "enum" => json!({"type":"string","enum":field.values.clone().unwrap_or_default()}),
            _ => json!({"type":"string","maxLength":field.max_length.unwrap_or(256)}),
        };
        properties.insert(field.id.clone(), rule);
        if field.required && action.operation == Operation::RecordCreate {
            required.push(json!(field.id));
        }
    }
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn check_value(field: &Field, value: &Value) -> Result<()> {
    let invalid = || Error::rejected("record action input does not match its declared field");
    match field.field_type.as_str() {
        "tags" => {
            let items = value.as_array().ok_or_else(invalid)?;
            if items.len() > field.max_items.unwrap_or(16) as usize {
                return Err(invalid());
            }
            for item in items {
                let tag = item.as_str().ok_or_else(invalid)?;
                if tag.is_empty()
                    || tag.chars().count() > TAG_MAX_CHARS
                    || tag.chars().any(char::is_control)
                {
                    return Err(invalid());
                }
            }
        }
        _ => {
            let text = value.as_str().ok_or_else(invalid)?;
            if text.chars().count() > field.max_length.unwrap_or(256) as usize
                || text.chars().any(char::is_control)
                || field
                    .values
                    .as_ref()
                    .is_some_and(|allowed| !allowed.iter().any(|v| v == text))
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

/// The admitted input: host target and revision, plus declared field values.
struct Admitted {
    record_id: String,
    expected: Option<i64>,
    fields: Map<String, Value>,
}

/// Admit one invocation's input against the declared action. Unknown keys and
/// missing required fields refuse before anything is read or written.
fn admit(action: &Action, input: &Value) -> Result<Admitted> {
    let raw = input
        .as_object()
        .ok_or_else(|| Error::rejected("record action input must be an object"))?;
    let record_id = raw
        .get(RECORD_ID)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("record action needs a record id"))?;
    crate::proto::identifier(record_id, "record ID")?;
    let expected = match (action.operation, raw.get(EXPECTED_REVISION)) {
        (Operation::RecordCreate, None) => None,
        (Operation::RecordCreate, Some(_)) => {
            return Err(Error::rejected("create takes no expected revision"))
        }
        (Operation::RecordUpdate, Some(value)) => Some(
            value
                .as_i64()
                .filter(|n| *n >= 1)
                .ok_or_else(|| Error::rejected("expected record revision must be positive"))?,
        ),
        (Operation::RecordUpdate, None) => {
            return Err(Error::rejected("update needs the expected record revision"))
        }
    };
    let mut fields = Map::new();
    for (key, value) in raw {
        if key == RECORD_ID || key == EXPECTED_REVISION {
            continue;
        }
        let field = action
            .input
            .fields
            .iter()
            .find(|field| field.id == *key)
            .ok_or_else(|| Error::rejected("record action input has unsupported fields"))?;
        check_value(field, value)?;
        fields.insert(key.clone(), value.clone());
    }
    for field in action.input.fields.iter().filter(|f| f.required) {
        if !fields.contains_key(&field.id) && action.operation == Operation::RecordCreate {
            return Err(Error::rejected(
                "record action input is missing a required field",
            ));
        }
    }
    if action.operation == Operation::RecordUpdate && fields.is_empty() {
        return Err(Error::rejected("update needs at least one declared field"));
    }
    Ok(Admitted {
        record_id: record_id.to_string(),
        expected,
        fields,
    })
}

/// Build the stored profile. Create starts from a blank profile with unknown
/// consent; update starts from the stored profile so consent and every
/// unpatched field carry over. An empty optional text clears that field.
fn patched_profile(
    base: Option<&CustomerProfile>,
    fields: &Map<String, Value>,
) -> Result<CustomerProfile> {
    let mut value = match base {
        Some(profile) => {
            serde_json::to_value(profile).map_err(|e| Error::internal(e.to_string()))?
        }
        None => {
            json!({"schema":1,"display_name":"","email":null,"tags":[],"consent":{"email":"unknown"}})
        }
    };
    for (key, field_value) in fields {
        let stored = match field_value {
            Value::String(s) if s.is_empty() && key != "display_name" => Value::Null,
            other => other.clone(),
        };
        value[key] = stored;
    }
    CustomerProfile::parse(&value)
        .map_err(|_| Error::rejected("record profile exceeds its supported shape or bounds"))
}

/// The digest an approval binds: the approved declaration (its schema), the
/// install's descriptor digest, the operation and the immutable input.
fn operation_digest(descriptor_digest: &str, action: &Action, input: &Value) -> String {
    crate::store::app_runs::material_digest(&json!({
        "contract": "cadence-app-record-action/v1",
        "action": action.id,
        "operation": operation_name(action),
        "schema": input_schema(action),
        "descriptor_digest": descriptor_digest,
        "input": input,
    }))
}

/// The guarded declaration proof. Runs inside `with_runtime_snapshot`, which
/// holds the PM lock and has re-verified the bundle digest, so the install
/// cannot be upgraded, removed or revoked while the callback runs. Returns the
/// install digest, the declaration named `action_id`, and the verified files.
fn guarded_declaration<T>(
    shared: &Shared,
    install: &str,
    context: &str,
    action_id: &str,
    work: impl FnOnce(&str, &Action) -> Result<T>,
) -> Result<T> {
    let pm = shared.pm_at(&shared.pm_dir()?)?;
    workspace::with_runtime_snapshot(&pm, install, |row, files| {
        let digest = row
            .get("digest")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("installation digest unavailable"))?
            .to_string();
        let text = files.get(crate::issue::app_action::FILE).ok_or_else(|| {
            Error::rejected(
                "unknown assistant action: action is not declared by this consented installation",
            )
        })?;
        // Serialize against the release/consent writers that withdraw authority.
        let _release = shared
            .app_release_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Re-prove the requested context under the release lock: an archive
        // takes the same lock, so it cannot land between this check and the work.
        shared.store.app_context_proof(install, context)?;
        let state = shared.store.app_capability_status(install, &digest)?;
        if state.get("state").and_then(Value::as_str) != Some("approved") {
            return Err(Error::rejected(
                "record action declaration is not consented",
            ));
        }
        let app = shared.install_app_slug(install)?;
        let descriptor = crate::issue::app_action::validate_customer_bundle(text, &app)?;
        let action = declared_action(&descriptor, action_id).ok_or_else(|| {
            Error::rejected(
                "unknown assistant action: action is not declared by this consented installation",
            )
        })?;
        work(&digest, &action)
    })
}

/// Discovery: the approved declarations, as the chat's action list shows them.
/// Read-only, so it uses the lock-free read; the host registry is untouched.
pub(super) fn declared_record_actions(shared: &Shared, install: &str) -> Result<Vec<Value>> {
    let pm = shared.pm_at(&shared.pm_dir()?)?;
    workspace::with_runtime_read(&pm, install, |row, files| {
        let Some(text) = files.get(crate::issue::app_action::FILE) else {
            return Ok(Vec::new());
        };
        let digest = row.get("digest").and_then(Value::as_str).unwrap_or("");
        let state = shared.store.app_capability_status(install, digest)?;
        if state.get("state").and_then(Value::as_str) != Some("approved") {
            return Ok(Vec::new());
        }
        let app = shared.install_app_slug(install)?;
        let full = shared.store.crm_autonomy_is_full()?;
        let descriptor = crate::issue::app_action::validate_customer_bundle(text, &app)?;
        Ok(descriptor
            .actions
            .iter()
            .map(|action| {
                json!({"id":action.id,"description":action.title,"input_schema":input_schema(action),"effect":"write","confirmation":if full {"none"} else {"permission_required"},"availability":"available"})
            })
            .collect())
    })
}

/// The preview of one proposed change, from the admitted input and the
/// current stored record. Create requires the id to be free; update requires
/// the expected revision to be current.
fn build_preview(
    records: &RecordStore,
    context: &str,
    action: &Action,
    admitted: &Admitted,
) -> Result<Value> {
    let (before, revision) = match action.operation {
        Operation::RecordCreate => {
            if records
                .app_record_show(context, &admitted.record_id)
                .is_ok()
            {
                return Err(Error::rejected("record id already exists; create refused"));
            }
            (Value::Null, 1)
        }
        Operation::RecordUpdate => {
            let current = records.app_record_show(context, &admitted.record_id)?;
            let revision = current
                .pointer("/record/revision")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::rejected("record revision is unavailable"))?;
            if admitted.expected != Some(revision) {
                return Err(Error::rejected("record revision is stale"));
            }
            (current["record"]["profile"].clone(), revision)
        }
    };
    // A standing tags-only deny is a statement about this customer's
    // tags, not about one action id: a declared record update that
    // carries tags must not route around it.
    if action.operation == Operation::RecordUpdate
        && admitted.fields.contains_key("tags")
        && records
            .app_assistant_permission_find(
                context,
                "customer.tags.update",
                &admitted.record_id,
                "deny",
            )?
            .is_some_and(|permission| permission["state"].as_str() == Some("active"))
    {
        return Err(Error::rejected("this action is blocked for this customer"));
    }
    let base = if before.is_null() {
        None
    } else {
        Some(
            CustomerProfile::parse(&before)
                .map_err(|_| Error::rejected("record profile cannot be safely updated"))?,
        )
    };
    let after = patched_profile(base.as_ref(), &admitted.fields)?;
    Ok(json!({
        "record_id": admitted.record_id,
        "operation": operation_name(action),
        "before": before,
        "after": serde_json::to_value(&after).map_err(|e| Error::internal(e.to_string()))?,
        "expected_revision": revision,
    }))
}

/// Agent invocation: admit the input and park a `pending_permission`
/// operation with the authoritative preview. Writes no record.
pub(super) fn invoke_record_action(
    shared: &Shared,
    records: &RecordStore,
    scoped: &ScopedChat,
    operation_id: &str,
    action_id: &str,
    input: &Value,
) -> Result<Value> {
    guarded_declaration(
        shared,
        &scoped.install,
        &scoped.context,
        action_id,
        |digest, action| {
            let semantic = operation_digest(digest, action, input);
            let (op, is_new) = records.app_assistant_operation_create(
                operation_id,
                &scoped.context,
                action_id,
                &semantic,
                input,
            )?;
            if !is_new || op.get("status").and_then(Value::as_str) != Some("running") {
                return Ok(json!({"operation": super::app_assistant_rpc::public_operation(op)}));
            }
            if shared.store.crm_autonomy_is_full()? {
                return apply_record_action_auto(
                    records,
                    &scoped.context,
                    operation_id,
                    action,
                    input,
                    &scoped.caller,
                );
            }
            let proposed = admit(action, input).and_then(|admitted| {
                let preview = build_preview(records, &scoped.context, action, &admitted)?;
                Ok((admitted.record_id, preview))
            });
            match proposed {
                Ok((record_id, preview)) => {
                    let reason = format!(
                        "{} {}: review the exact change before allowing it once",
                        action.title, record_id
                    );
                    let op = records.app_assistant_operation_set(AssistantOperationUpdate {
                    operation_id,
                    context: &scoped.context,
                    expected_revision: 1,
                    status: "pending_permission",
                    summary: "Review this customer change",
                    result: &json!({"preview": preview}),
                    resource_refs: &json!([{"kind":"customer","id":record_id,"label":record_id}]),
                    permission_request: &json!({"reason":reason,"scope":{"install_id":scoped.install,"context_id":scoped.context,"action_id":action_id,"resource_id":record_id},"allow_always":false,"preview_kind":"record","preview":preview}),
                    error: None,
                })?;
                    Ok(json!({"operation": super::app_assistant_rpc::public_operation(op)}))
                }
                Err(error) => {
                    // Refused before any write: a truthful no-change failure.
                    let _ = records.app_assistant_operation_set(AssistantOperationUpdate {
                        operation_id,
                        context: &scoped.context,
                        expected_revision: 1,
                        status: "failed",
                        summary: "Action was refused",
                        result: &Value::Null,
                        resource_refs: &json!([]),
                        permission_request: &Value::Null,
                        error: Some("Action was refused without a recorded change"),
                    });
                    Err(error)
                }
            }
        },
    )
}

/// Autonomous record write at `full` autonomy: the same admission,
/// preview and compare-and-set as the operator path, but the scoped
/// agent turn is the actor. No permission row is created.
fn apply_record_action_auto(
    records: &RecordStore,
    context: &str,
    operation_id: &str,
    action: &Action,
    input: &Value,
    caller: &str,
) -> Result<Value> {
    let actor = format!("agent:{caller}");
    let attempt = (|| -> Result<(String, CustomerProfile, Value)> {
        let admitted = admit(action, input)?;
        let preview = build_preview(records, context, action, &admitted)?;
        let base = match action.operation {
            Operation::RecordCreate => None,
            Operation::RecordUpdate => Some(
                CustomerProfile::parse(&preview["before"])
                    .map_err(|_| Error::rejected("record profile cannot be safely updated"))?,
            ),
        };
        let profile = patched_profile(base.as_ref(), &admitted.fields)?;
        match action.operation {
            Operation::RecordCreate => records.app_record_create_as_actor(
                context,
                &admitted.record_id,
                &profile,
                None,
                &actor,
                None,
            ),
            Operation::RecordUpdate => records.app_record_update_as_actor(
                context,
                &admitted.record_id,
                admitted.expected.unwrap_or_default(),
                &profile,
                None,
                &actor,
                None,
            ),
        }
        .map(|stored| (admitted.record_id, profile, stored))
    })();
    match attempt {
        Ok((record_id, profile, stored)) => {
            let revision = stored
                .pointer("/record/revision")
                .cloned()
                .unwrap_or(Value::Null);
            let op = records.app_assistant_operation_set(AssistantOperationUpdate {
                operation_id,
                context,
                expected_revision: 1,
                status: "succeeded",
                summary: "Record change applied",
                result: &json!({"record_id": record_id, "revision": revision}),
                resource_refs: &json!([{"kind":"customer","id":record_id,"label":profile.display_name}]),
                permission_request: &Value::Null,
                error: None,
            })?;
            Ok(json!({"operation": super::app_assistant_rpc::public_operation(op)}))
        }
        Err(error) => {
            let definite = super::app_assistant_rpc::assistant_error_is_definite_refusal(&error);
            let (status, summary, detail) = if definite {
                (
                    "failed",
                    "Record change was refused; no change was made",
                    "Action was refused without a recorded change",
                )
            } else {
                (
                    "unknown",
                    "Outcome uncertain; operator must verify the record before any retry",
                    "Outcome may be uncertain; do not retry automatically",
                )
            };
            let _ = records.app_assistant_operation_set(AssistantOperationUpdate {
                operation_id,
                context,
                expected_revision: 1,
                status,
                summary,
                result: &Value::Null,
                resource_refs: &json!([]),
                permission_request: &Value::Null,
                error: Some(detail),
            });
            Err(error)
        }
    }
}

/// Native operator decision for a declared record action (`allow_once` only).
///
/// Ordering: PM lock and bundle digest (`with_runtime_snapshot`), then the
/// release lock and consent, then the operation CAS to `running`, then the
/// record write, then the settle CAS. Each SQLite call takes and drops its own
/// connection guard, so nothing nests. The state machine:
/// - refusal before the write settles `failed` (no change, retry permitted
///   only by a new request);
/// - a write that may have committed without a proven receipt settles
///   `unknown` and is never retried automatically.
pub(super) fn decide_record_action(
    shared: &Shared,
    records: &RecordStore,
    install: &str,
    context: &str,
    op: &Value,
    operation_id: &str,
    expected: i64,
) -> Result<Value> {
    let input = op
        .get("_input")
        .ok_or_else(|| Error::rejected("assistant operation input is unavailable"))?;
    let action_id = op["action_id"]
        .as_str()
        .ok_or_else(|| Error::rejected("assistant operation action is unavailable"))?;
    guarded_declaration(shared, install, context, action_id, |digest, action| {
        if op.get("_operation_digest").and_then(Value::as_str)
            != Some(operation_digest(digest, action, input).as_str())
        {
            return Err(Error::rejected(
                "record action semantics changed; permission must be requested again",
            ));
        }
        let admitted = admit(action, input)?;
        let preview = op
            .pointer("/permission_request/preview")
            .ok_or_else(|| Error::rejected("assistant permission preview is unavailable"))?;
        let fresh = build_preview(records, context, action, &admitted).inspect_err(|_| {
            // Refused before the write: settle a truthful no-change failure.
            let _ = records.app_assistant_operation_set(AssistantOperationUpdate {
                operation_id,
                context,
                expected_revision: expected,
                status: "failed",
                summary: "Record changed since the preview; no change was made",
                result: &Value::Null,
                resource_refs: &json!([]),
                permission_request: &Value::Null,
                error: Some("Record changed since the preview; no change was made"),
            });
        })?;
        if preview.get("record_id") != fresh.get("record_id")
            || preview.get("before") != fresh.get("before")
            || preview.get("after") != fresh.get("after")
            || preview.get("expected_revision") != fresh.get("expected_revision")
        {
            let _ = records.app_assistant_operation_set(AssistantOperationUpdate {
                operation_id,
                context,
                expected_revision: expected,
                status: "failed",
                summary: "Record changed since the preview; no change was made",
                result: &Value::Null,
                resource_refs: &json!([]),
                permission_request: &Value::Null,
                error: Some("Record changed since the preview; no change was made"),
            });
            return Err(Error::rejected(
                "record changed since permission preview; review the updated impact",
            ));
        }
        // Advance pending -> running under CAS before any write. A stale
        // revision here means nothing changed; the error is returned as is.
        let running = records.app_assistant_operation_set(AssistantOperationUpdate {
            operation_id,
            context,
            expected_revision: expected,
            status: "running",
            summary: "Applying the approved record change",
            result: &Value::Null,
            resource_refs: &json!([]),
            permission_request: &Value::Null,
            error: None,
        })?;
        let running_revision = running["revision"]
            .as_i64()
            .ok_or_else(|| Error::internal("assistant operation revision is unavailable"))?;
        let base = match action.operation {
            Operation::RecordCreate => None,
            Operation::RecordUpdate => Some(
                CustomerProfile::parse(&fresh["before"])
                    .map_err(|_| Error::rejected("record profile cannot be safely updated"))?,
            ),
        };
        let profile = patched_profile(base.as_ref(), &admitted.fields)?;
        let written = match action.operation {
            Operation::RecordCreate => {
                records.app_record_create(context, &admitted.record_id, &profile)
            }
            Operation::RecordUpdate => records.app_record_update(
                context,
                &admitted.record_id,
                admitted.expected.unwrap_or_default(),
                &profile,
                None,
            ),
        };
        match written {
            Ok(stored) => {
                let result = records.app_assistant_operation_set(AssistantOperationUpdate {
                    operation_id,
                    context,
                    expected_revision: running_revision,
                    status: "succeeded",
                    summary: "Record change applied",
                    result: &json!({"record_id": admitted.record_id, "revision": stored.pointer("/record/revision")}),
                    resource_refs: &json!([{"kind":"customer","id":admitted.record_id,"label":profile.display_name}]),
                    permission_request: &Value::Null,
                    error: None,
                })?;
                Ok(json!({"operation": result}))
            }
            Err(error) => {
                // The store rejected the write before commit, so nothing
                // changed; a definite refusal settles failed. Any other error
                // may have committed, so it stays unknown and is not retried.
                let definite =
                    super::app_assistant_rpc::assistant_error_is_definite_refusal(&error);
                let (status, summary, detail) = if definite {
                    (
                        "failed",
                        "Record change was refused; no change was made",
                        "Action was refused without a recorded change",
                    )
                } else {
                    (
                        "unknown",
                        "Outcome uncertain; operator must verify the record before any retry",
                        "Outcome may be uncertain; do not retry automatically",
                    )
                };
                let _ = records.app_assistant_operation_set(AssistantOperationUpdate {
                    operation_id,
                    context,
                    expected_revision: running_revision,
                    status,
                    summary,
                    result: &Value::Null,
                    resource_refs: &json!([]),
                    permission_request: &Value::Null,
                    error: Some(detail),
                });
                Err(error)
            }
        }
    })
}
