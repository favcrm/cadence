//! Action JSON schema registry and input validation for `app_assistant`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::{json, Value};

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

pub(super) fn schema(id: &str) -> Option<Value> {
    Some(match id {
        "customers.search" => object(
            json!({"query":{"type":"string","maxLength":160},"limit":{"type":"integer","minimum":1,"maximum":20},"cursor":{"type":"string","maxLength":256}}),
            &[],
        ),
        "customers.show" => object(
            json!({"customer_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["customer_id"],
        ),
        "customers.notes.list" => object(
            json!({"customer_id":{"type":"string","minLength":1,"maxLength":128},"limit":{"type":"integer","minimum":1,"maximum":50}}),
            &["customer_id"],
        ),
        "customers.notes.create" => object(
            json!({"customer_id":{"type":"string","minLength":1,"maxLength":128},"note_id":{"type":"string","minLength":1,"maxLength":128},"body":{"type":"string","minLength":1,"maxLength":4000}}),
            &["customer_id", "note_id", "body"],
        ),
        "customers.notes.delete" => object(
            json!({"customer_id":{"type":"string","minLength":1,"maxLength":128},"note_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["customer_id", "note_id"],
        ),
        "segments.list" | "campaigns.list" => object(json!({}), &[]),
        "segments.show" => object(
            json!({"segment_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["segment_id"],
        ),
        "segments.preview" => object(
            json!({"segment_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["segment_id"],
        ),
        "segments.save" => object(
            json!({"segment_id":{"type":"string","minLength":1,"maxLength":128},"name":{"type":"string","minLength":1,"maxLength":120},"predicates":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","properties":{"field":{"type":"string"},"op":{"type":"string"},"value":{}},"required":["field","op","value"],"additionalProperties":false}},"expected_revision":{"type":"integer","minimum":1}}),
            &["segment_id", "name", "predicates"],
        ),
        "segments.save_static" => object(
            json!({"segment_id":{"type":"string","minLength":1,"maxLength":128},"name":{"type":"string","minLength":1,"maxLength":120},"member_ids":{"type":"array","maxItems":1000,"items":{"type":"string","minLength":1,"maxLength":128}},"expected_revision":{"type":"integer","minimum":1}}),
            &["segment_id", "name", "member_ids"],
        ),
        "segments.delete" => object(
            json!({"segment_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["segment_id"],
        ),
        "campaigns.apply" => object(
            json!({"proposal_id":{"type":"string","minLength":1,"maxLength":128},"expected_revision":{"type":"integer","minimum":1}}),
            &["proposal_id"],
        ),
        "campaigns.discard" => object(
            json!({"proposal_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["proposal_id"],
        ),
        "campaigns.approve" => object(
            json!({"campaign_id":{"type":"string","minLength":1,"maxLength":128},"expected_revision":{"type":"integer","minimum":1}}),
            &["campaign_id", "expected_revision"],
        ),
        "audiences.prepare" => object(
            json!({"freeze_id":{"type":"string","minLength":1,"maxLength":128},"base":{"type":"object"},"exclusion_list_id":{"type":"string","maxLength":128},"max_recipients":{"type":"integer","minimum":1,"maximum":500}}),
            &["freeze_id", "base", "max_recipients"],
        ),
        "audiences.show" => object(
            json!({"freeze_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["freeze_id"],
        ),
        "sends.list" => object(
            json!({"campaign_id":{"type":"string","minLength":1,"maxLength":128}}),
            &[],
        ),
        "sends.show" => object(
            json!({"send_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["send_id"],
        ),
        "campaigns.show" => object(
            json!({"campaign_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["campaign_id"],
        ),
        "campaigns.create_draft" => object(
            json!({"campaign_id":{"type":"string","minLength":1,"maxLength":128},"name":{"type":"string","minLength":1,"maxLength":120},"segment_id":{"type":"string","maxLength":128}}),
            &["campaign_id", "name"],
        ),
        "email.draft" => object(
            json!({"campaign_id":{"type":"string","minLength":1,"maxLength":128},"proposal_id":{"type":"string","minLength":1,"maxLength":128},"draft":{"type":"object"}}),
            &["campaign_id", "proposal_id", "draft"],
        ),
        "customer.tags.update" => object(
            json!({"customer_id":{"type":"string","minLength":1,"maxLength":128},"tags":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":40}},"expected_revision":{"type":"integer","minimum":1}}),
            &["customer_id", "tags", "expected_revision"],
        ),
        "social.posts.fetch" => object(
            json!({"handle":{"type":"string","minLength":1,"maxLength":64}}),
            &[],
        ),
        "social.draft.create" => object(
            json!({"post":{"type":"string","minLength":1,"maxLength":128},"instructions":{"type":"string","maxLength":500},"with_image":{"type":"boolean"}}),
            &["post"],
        ),
        "social.draft.compose" => object(
            json!({"caption":{"type":"string","minLength":1,"maxLength":8800},"brief":{"type":"string","maxLength":500},"asset_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["caption"],
        ),
        "social.drafts.attach_image" => object(
            json!({"draft_id":{"type":"string","minLength":1,"maxLength":128},"expected_revision":{"type":"integer","minimum":1},"asset_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["draft_id", "expected_revision", "asset_id"],
        ),
        "social.drafts.status" => object(json!({}), &[]),
        "media.refine.prepare" => object(
            json!({"asset_id":{"type":"string","minLength":1,"maxLength":64},"revision":{"type":"integer","minimum":1},"sha256":{"type":"string","minLength":71,"maxLength":71},"instructions":{"type":"string","minLength":1,"maxLength":2000}}),
            &["asset_id", "revision", "sha256", "instructions"],
        ),
        // CAD-1636: the WhatsApp copilot. The registry validator counts
        // bytes, so draft text gets a 4000-byte transport bound and the
        // handler enforces the design's 1000-character bound.
        "whatsapp.conversation.context" => object(
            json!({"conversation_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["conversation_id"],
        ),
        "whatsapp.reply.draft" => object(
            json!({"conversation_id":{"type":"string","minLength":1,"maxLength":128},"draft_id":{"type":"string","minLength":1,"maxLength":128},"text":{"type":"string","minLength":1,"maxLength":4000},"basis_message_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["conversation_id", "draft_id", "text", "basis_message_id"],
        ),
        _ => return None,
    })
}

/// The consented package contract must agree exactly with the host registry;
/// discovery and invocation therefore cannot drift to unhandled fields.
pub fn schema_accepts(id: &str, descriptor: &Value) -> bool {
    if schema(id).is_none() {
        return false;
    }
    let Some(registered) = schema(id) else {
        return false;
    };
    descriptor == &registered
}

pub fn validate_input(schema: &Value, input: &Value) -> crate::Result<()> {
    let fields = input
        .as_object()
        .ok_or_else(|| crate::Error::rejected("assistant action input must be an object"))?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| crate::Error::internal("registered action schema is invalid"))?;
    if schema.get("additionalProperties").and_then(Value::as_bool) != Some(false)
        || fields.keys().any(|key| !properties.contains_key(key))
    {
        return Err(crate::Error::rejected(
            "assistant action input has unsupported fields",
        ));
    }
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = required
            .as_str()
            .ok_or_else(|| crate::Error::internal("registered action schema is invalid"))?;
        if !fields.contains_key(key) {
            return Err(crate::Error::rejected(
                "assistant action input is missing a required field",
            ));
        }
    }
    for (key, value) in fields {
        let rule = properties.get(key).ok_or_else(|| {
            crate::Error::rejected("assistant action input has unsupported fields")
        })?;
        validate_value(rule, value)?;
    }
    Ok(())
}

fn validate_value(rule: &Value, value: &Value) -> crate::Result<()> {
    if rule.as_object().is_some_and(|object| object.is_empty()) {
        return Ok(());
    }
    let invalid =
        || crate::Error::rejected("assistant action input does not match the registered schema");
    match rule.get("type").and_then(Value::as_str).unwrap_or("object") {
        "string" => {
            let text = value.as_str().ok_or_else(invalid)?;
            if text.len()
                > rule
                    .get("maxLength")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX) as usize
                || text.len() < rule.get("minLength").and_then(Value::as_u64).unwrap_or(0) as usize
            {
                return Err(invalid());
            }
        }
        "boolean" => {
            value.as_bool().ok_or_else(invalid)?;
        }
        "integer" => {
            let number = value.as_i64().ok_or_else(invalid)?;
            if number
                < rule
                    .get("minimum")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::MIN)
                || number
                    > rule
                        .get("maximum")
                        .and_then(Value::as_i64)
                        .unwrap_or(i64::MAX)
            {
                return Err(invalid());
            }
        }
        "array" => {
            let items = value.as_array().ok_or_else(invalid)?;
            if items.len() < rule.get("minItems").and_then(Value::as_u64).unwrap_or(0) as usize
                || items.len()
                    > rule
                        .get("maxItems")
                        .and_then(Value::as_u64)
                        .unwrap_or(u64::MAX) as usize
            {
                return Err(invalid());
            }
            if let Some(item_rule) = rule.get("items") {
                for item in items {
                    validate_value(item_rule, item)?;
                }
            }
        }
        "object" => {
            let object = value.as_object().ok_or_else(invalid)?;
            if rule
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(|properties| !properties.is_empty())
            {
                validate_input(rule, value)?;
            } else if object.is_empty() || rule.get("properties").is_none() {
                // Deliberately unconstrained object (e.g. the draft payload);
                // its owning adapter applies the domain-specific shape.
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
