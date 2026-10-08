//! Host-owned registry for the first generic app assistant action set.
use serde_json::{json, Value};

#[derive(Clone)]
pub struct RegisteredAction {
    pub id: &'static str,
    pub effect: &'static str,
    pub confirmation: &'static str,
    pub handler_version: &'static str,
    pub input_schema: Value,
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn schema(id: &str) -> Option<Value> {
    Some(match id {
        "customers.search" => object(
            json!({"query":{"type":"string","maxLength":160},"limit":{"type":"integer","minimum":1,"maximum":20},"cursor":{"type":"string","maxLength":256}}),
            &[],
        ),
        "customers.show" => object(
            json!({"customer_id":{"type":"string","minLength":1,"maxLength":128}}),
            &["customer_id"],
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

pub fn registered_action(id: &str) -> Option<RegisteredAction> {
    let (effect, confirmation) = match id {
        "customers.search" | "customers.show" | "segments.list" | "segments.show"
        | "segments.preview" | "campaigns.list" | "campaigns.show" => ("read", "none"),
        "segments.save" => ("write", "none"),
        "campaigns.create_draft" => ("draft", "none"),
        "email.draft" => ("proposal", "none"),
        "customer.tags.update" => ("write", "permission_required"),
        _ => return None,
    };
    Some(RegisteredAction {
        id: match id {
            "customers.search" => "customers.search",
            "customers.show" => "customers.show",
            "segments.list" => "segments.list",
            "segments.show" => "segments.show",
            "segments.preview" => "segments.preview",
            "segments.save" => "segments.save",
            "campaigns.list" => "campaigns.list",
            "campaigns.show" => "campaigns.show",
            "campaigns.create_draft" => "campaigns.create_draft",
            "email.draft" => "email.draft",
            "customer.tags.update" => "customer.tags.update",
            _ => unreachable!(),
        },
        effect,
        confirmation,
        handler_version: "v1",
        input_schema: schema(id)?,
    })
}

pub fn registered_ids() -> &'static [&'static str] {
    &[
        "customers.search",
        "customers.show",
        "segments.list",
        "segments.show",
        "segments.preview",
        "segments.save",
        "campaigns.list",
        "campaigns.show",
        "campaigns.create_draft",
        "email.draft",
        "customer.tags.update",
    ]
}
