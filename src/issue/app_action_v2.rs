//! Installed app-actions/v2 descriptor: strict data admission plus the
//! first deliberately closed live adapter, CRM customer create/update.
//! V1 action metadata remains inert; every write re-proves this descriptor
//! with the view and binding from one verified installation snapshot.

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::issue::{app::Manifest, app_action, app_binding, app_view};

pub const CONTRACT: &str = "app-actions/v2";
pub const FILE: &str = "app-actions-v2.json";
pub const REL_PATH: &str = "actions/app-actions-v2.json";

const MAX_SERIALIZED_BYTES: usize = 64 * 1024;
const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 24;
const MAX_FIELDS: usize = 64;
const FIELD_KEYS: &[&str] = &[
    "id",
    "label",
    "type",
    "required",
    "nullable",
    "values",
    "maxLength",
    "maxItems",
];
const V2_FORBIDDEN_KEYS: &[&str] = &[
    "consent",
    "consent_email",
    "consent_sms",
    "record_id",
    "recordId",
    "expected_revision",
    "expectedRevision",
];
const CUSTOMER_FIELDS: &[(&str, &str, bool, bool)] = &[
    ("display_name", "text", true, false),
    ("email", "text", false, true),
    ("phone", "text", false, true),
    ("source", "text", false, true),
    ("tags", "tags", false, false),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub id: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    pub nullable: bool,
    pub values: Option<Vec<String>>,
    pub max_length: Option<u32>,
    pub max_items: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub operation: app_action::Operation,
    pub record: String,
    pub form_view: String,
    pub input: Input,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Descriptor {
    pub app: String,
    pub title: String,
    pub summary: Option<String>,
    pub actions: Vec<Action>,
    pub raw: Value,
}

fn fail(path: &str, message: impl std::fmt::Display) -> Error {
    Error::rejected(format!("app-actions/v2 {path}: {message}"))
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| fail(path, "expected an object"))
}

fn closed(map: &Map<String, Value>, allowed: &[&str], path: &str) -> Result<()> {
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(fail(path, format!("unknown key: {key}")));
    }
    Ok(())
}

fn identifier(value: &Value, path: &str) -> Result<String> {
    let text = value
        .as_str()
        .ok_or_else(|| fail(path, "expected an identifier"))?;
    if text.is_empty()
        || text.len() > 64
        || !text.starts_with(|c: char| c.is_ascii_lowercase())
        || !text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
    {
        return Err(fail(path, "identifier is outside the closed grammar"));
    }
    Ok(text.to_string())
}

fn bounded_text(value: &Value, path: &str, max: usize) -> Result<()> {
    let text = value.as_str().ok_or_else(|| fail(path, "expected text"))?;
    if text.is_empty()
        || text.chars().count() > max
        || text
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(fail(
            path,
            format!("expected bounded non-empty text (max {max})"),
        ));
    }
    Ok(())
}

fn scan_forbidden(value: &Value, path: &str, nodes: &mut usize, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(fail(path, "input is nested too deeply"));
    }
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err(fail(path, "input has too many nodes"));
    }
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                scan_forbidden(item, &format!("{path}.{index}"), nodes, depth + 1)?;
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                if V2_FORBIDDEN_KEYS.contains(&key.as_str())
                    || app_action::FORBIDDEN_DESCRIPTOR_KEYS.contains(&key.as_str())
                {
                    return Err(fail(path, format!("forbidden descriptor key: {key}")));
                }
                scan_forbidden(child, &format!("{path}.{key}"), nodes, depth + 1)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn v1_compatible(raw: &Value) -> Result<Value> {
    let mut compatible = raw.clone();
    let root = compatible
        .as_object_mut()
        .ok_or_else(|| fail("$", "descriptor must be an object"))?;
    root.insert(
        "contract".into(),
        Value::String(app_action::CONTRACT.into()),
    );
    let actions = root
        .get_mut("actions")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| fail("$.actions", "expected an array"))?;
    for action in actions {
        let map = action
            .as_object_mut()
            .ok_or_else(|| fail("$.actions", "action must be an object"))?;
        map.remove("form_view");
        let fields = map
            .get_mut("input")
            .and_then(Value::as_object_mut)
            .and_then(|input| input.get_mut("fields"))
            .and_then(Value::as_array_mut)
            .ok_or_else(|| fail("$.actions.input.fields", "expected an array"))?;
        for field in fields {
            field
                .as_object_mut()
                .ok_or_else(|| fail("$.actions.input.fields", "field must be an object"))?
                .remove("nullable");
        }
    }
    Ok(compatible)
}

fn parse_nullable(field: &Value, path: &str) -> Result<bool> {
    let nullable = match field.get("nullable") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(fail(&format!("{path}.nullable"), "expected a boolean")),
    };
    if nullable {
        let required = field.get("required") == Some(&Value::Bool(true));
        let field_type = field.get("type").and_then(Value::as_str).unwrap_or("text");
        if required || field_type != "text" {
            return Err(fail(
                &format!("{path}.nullable"),
                "nullable is only allowed on optional text fields",
            ));
        }
    }
    Ok(nullable)
}

/// Parse the app-actions/v1 bounded descriptor grammar plus v2's required
/// form_view reference and optional-text nullable flag.
pub fn parse(raw: &Value) -> Result<Descriptor> {
    let mut nodes = 0;
    scan_forbidden(raw, "$", &mut nodes, 0)?;
    let serialized = serde_json::to_vec(raw).map_err(|error| fail("$", error))?;
    if serialized.len() > MAX_SERIALIZED_BYTES {
        return Err(fail(
            "$",
            format!("input exceeds {MAX_SERIALIZED_BYTES} bytes"),
        ));
    }
    let root = object(raw, "$")?;
    closed(
        root,
        &["contract", "app", "title", "summary", "actions"],
        "$",
    )?;
    if root.get("contract") != Some(&Value::String(CONTRACT.into())) {
        return Err(fail("$.contract", format!("expected {CONTRACT:?}")));
    }
    bounded_text(
        root.get("app")
            .ok_or_else(|| fail("$.app", "missing app"))?,
        "$.app",
        64,
    )?;
    bounded_text(
        root.get("title")
            .ok_or_else(|| fail("$.title", "missing title"))?,
        "$.title",
        120,
    )?;
    if let Some(summary) = root.get("summary") {
        bounded_text(summary, "$.summary", 280)?;
    }
    let actions = root
        .get("actions")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("$.actions", "expected an array"))?;
    if actions.is_empty() || actions.len() > 16 {
        return Err(fail("$.actions", "expected 1–16 actions"));
    }
    let mut action_ids = std::collections::HashSet::new();
    for (index, action) in actions.iter().enumerate() {
        let path = format!("$.actions.{index}");
        let map = object(action, &path)?;
        closed(
            map,
            &["id", "title", "operation", "record", "form_view", "input"],
            &path,
        )?;
        bounded_text(
            map.get("title")
                .ok_or_else(|| fail(&path, "missing title"))?,
            &format!("{path}.title"),
            120,
        )?;
        identifier(
            map.get("form_view").unwrap_or(&Value::Null),
            &format!("{path}.form_view"),
        )?;
        let id = map
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| fail(&path, "missing id"))?;
        if !action_ids.insert(id) {
            return Err(fail("$.actions", format!("duplicate action id: {id}")));
        }
        let input = object(
            map.get("input").unwrap_or(&Value::Null),
            &format!("{path}.input"),
        )?;
        closed(input, &["fields"], &format!("{path}.input"))?;
        let fields = input
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| fail(&format!("{path}.input.fields"), "expected an array"))?;
        if fields.is_empty() || fields.len() > MAX_FIELDS {
            return Err(fail(
                &format!("{path}.input.fields"),
                format!("expected 1–{MAX_FIELDS} fields"),
            ));
        }
        let mut field_ids = std::collections::HashSet::new();
        for (field_index, field) in fields.iter().enumerate() {
            let field_path = format!("{path}.input.fields.{field_index}");
            let field_map = object(field, &field_path)?;
            closed(field_map, FIELD_KEYS, &field_path)?;
            identifier(
                field_map.get("id").unwrap_or(&Value::Null),
                &format!("{field_path}.id"),
            )?;
            bounded_text(
                field_map
                    .get("label")
                    .ok_or_else(|| fail(&field_path, "missing label"))?,
                &format!("{field_path}.label"),
                80,
            )?;
            if !field_ids.insert(
                field_map
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ) {
                return Err(fail(&field_path, "duplicate field id"));
            }
            parse_nullable(field, &field_path)?;
        }
    }
    let compatible = v1_compatible(raw)?;
    let parsed = app_action::parse(&compatible).map_err(|error| match error {
        Error::Rejected(message) => {
            Error::rejected(message.replace("app-actions/v1", "app-actions/v2"))
        }
        other => other,
    })?;
    let mut parsed_actions = Vec::with_capacity(parsed.actions.len());
    for (index, action) in parsed.actions.into_iter().enumerate() {
        let raw_action = &actions[index];
        let form_view = identifier(
            raw_action.get("form_view").unwrap_or(&Value::Null),
            &format!("$.actions.{index}.form_view"),
        )?;
        let raw_fields = raw_action["input"]["fields"].as_array().ok_or_else(|| {
            fail(
                &format!("$.actions.{index}.input.fields"),
                "expected an array",
            )
        })?;
        let fields = action
            .input
            .fields
            .into_iter()
            .zip(raw_fields)
            .map(|(field, raw_field)| {
                Ok(Field {
                    id: field.id,
                    label: field.label,
                    field_type: field.field_type,
                    required: field.required,
                    nullable: raw_field.get("nullable") == Some(&Value::Bool(true)),
                    values: field.values,
                    max_length: field.max_length,
                    max_items: field.max_items,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        parsed_actions.push(Action {
            id: action.id,
            title: action.title,
            operation: action.operation,
            record: action.record,
            form_view,
            input: Input { fields },
        });
    }
    Ok(Descriptor {
        app: parsed.app,
        title: parsed.title,
        summary: parsed.summary,
        actions: parsed_actions,
        raw: raw.clone(),
    })
}

pub fn parse_str(text: &str) -> Result<Descriptor> {
    if text.len() > MAX_SERIALIZED_BYTES {
        return Err(fail(
            "$",
            format!("input exceeds {MAX_SERIALIZED_BYTES} bytes"),
        ));
    }
    let raw: Value = serde_json::from_str(text)
        .map_err(|error| fail("$", format!("not valid JSON: {error}")))?;
    parse(&raw)
}

fn expected_customer_field(field: &Field, index: usize) -> bool {
    CUSTOMER_FIELDS
        .get(index)
        .is_some_and(|(id, kind, required, nullable)| {
            field.id == *id
                && field.field_type == *kind
                && field.required == *required
                && field.nullable == *nullable
                && field.values.is_none()
        })
}

fn preview_shape(field: &Field) -> (&str, &str) {
    match field.field_type.as_str() {
        "tags" => ("tags", "list"),
        other => (other, "scalar"),
    }
}

/// Validate the action, view and binding documents together. Only the two
/// CRM customer actions, their descriptor-declared inert form previews, and
/// the matching customers table/detail pair can use the first host adapter.
pub fn validate_against(
    descriptor: &Descriptor,
    manifest: &Manifest,
    views: Option<&app_view::Descriptor>,
    binding: Option<&app_binding::Binding>,
) -> Result<()> {
    if manifest.action_contract.as_deref() != Some(CONTRACT) {
        return Err(fail(
            "$.contract",
            "manifest does not declare app-actions/v2",
        ));
    }
    if descriptor.app != manifest.app {
        return Err(fail(
            "$.app",
            "action app differs from the installed manifest",
        ));
    }
    if descriptor.app != "crm" {
        return Err(fail(
            "$.app",
            "the v2 live adapter supports only the crm app",
        ));
    }
    let views = views.ok_or_else(|| fail("$", "actions require app-views/v1"))?;
    let binding = binding.ok_or_else(|| fail("$", "actions require app-bindings/v1"))?;
    if manifest.view_contract.as_deref() != Some(app_view::CONTRACT)
        || manifest.binding_contract.as_deref() != Some(app_binding::CONTRACT)
        || views.app != manifest.app
        || binding.app != manifest.app
    {
        return Err(fail(
            "$.app",
            "action, view and binding identities/contracts must match",
        ));
    }
    app_binding::validate_against(binding, manifest, Some(views))
        .map_err(|error| fail("$", format!("binding/descriptor pair is invalid: {error}")))?;
    if descriptor.actions.len() != 2 {
        return Err(fail(
            "$.actions",
            "the CRM adapter requires exactly customer.create and customer.update",
        ));
    }
    let mut action_ids = std::collections::HashSet::new();
    for action in &descriptor.actions {
        if !action_ids.insert(action.id.as_str()) {
            return Err(fail("$.actions", "duplicate action id"));
        }
        let operation = match action.id.as_str() {
            "customer.create" => app_action::Operation::RecordCreate,
            "customer.update" => app_action::Operation::RecordUpdate,
            _ => {
                return Err(fail(
                    "$.actions",
                    format!("unsupported action id: {}", action.id),
                ))
            }
        };
        if action.operation != operation || action.record != "customer" {
            return Err(fail(
                "$.actions",
                format!("{} has an unsupported operation or record", action.id),
            ));
        }
        if action.input.fields.len() != CUSTOMER_FIELDS.len()
            || !action
                .input
                .fields
                .iter()
                .enumerate()
                .all(|(index, field)| expected_customer_field(field, index))
        {
            return Err(fail(
                "$.actions.input.fields",
                "customer actions require the fixed display_name/email/phone/source/tags shape",
            ));
        }
        let form = views
            .views
            .iter()
            .find(|view| view.id == action.form_view)
            .ok_or_else(|| {
                fail(
                    "$.actions.form_view",
                    format!("undeclared form preview: {}", action.form_view),
                )
            })?;
        if form.kind != "form" || form.preview_of.len() != action.input.fields.len() {
            return Err(fail(
                "$.actions.form_view",
                "action must select a matching form preview",
            ));
        }
        for (index, (input, preview)) in
            action.input.fields.iter().zip(&form.preview_of).enumerate()
        {
            let (format, kind) = preview_shape(input);
            if preview.id != input.id
                || preview.label != input.label
                || preview.format != format
                || preview.kind != kind
                || preview.values != input.values
            {
                return Err(fail(
                    &format!("$.actions.{}.input.fields.{index}", action.id),
                    "action field does not match its paired preview",
                ));
            }
        }
    }
    if !action_ids.contains("customer.create") || !action_ids.contains("customer.update") {
        return Err(fail(
            "$.actions",
            "both customer.create and customer.update are required",
        ));
    }
    let customer_details = views
        .views
        .iter()
        .filter(|view| {
            view.kind == "detail"
                && binding.bindings.iter().any(|bound| {
                    bound.view == view.id
                        && bound.source == "customers"
                        && bound.ops.iter().any(|op| op == "show")
                })
        })
        .collect::<Vec<_>>();
    if customer_details.len() != 1 {
        return Err(fail(
            "$",
            "customer update needs exactly one bound customers detail view",
        ));
    }
    let detail_binding = binding
        .bindings
        .iter()
        .find(|bound| bound.view == customer_details[0].id)
        .ok_or_else(|| fail("$", "customer detail binding is absent"))?;
    for id in ["display_name", "email", "phone", "source", "tags"] {
        if detail_binding
            .fields
            .iter()
            .filter(|field| field.key == id)
            .count()
            != 1
        {
            return Err(fail(
                "$",
                format!("customer detail must project {id} exactly once"),
            ));
        }
    }
    let customer_tables = views
        .views
        .iter()
        .filter(|view| {
            view.kind == "table"
                && binding.bindings.iter().any(|bound| {
                    bound.view == view.id
                        && bound.source == "customers"
                        && bound.ops.iter().any(|op| op == "list")
                })
        })
        .collect::<Vec<_>>();
    if customer_tables.len() != 1 {
        return Err(fail(
            "$",
            "customer actions need exactly one bound customers table",
        ));
    }
    let table_binding = binding
        .bindings
        .iter()
        .find(|bound| bound.view == customer_tables[0].id)
        .ok_or_else(|| fail("$", "customer table binding is absent"))?;
    if table_binding
        .fields
        .iter()
        .filter(|field| field.key == "record_id" && field.format == "text")
        .count()
        != 1
    {
        return Err(fail(
            "$",
            "customer table needs exactly one bound record_id field",
        ));
    }
    if detail_binding.source != table_binding.source {
        return Err(fail(
            "$",
            "customer table and detail must use the same bound source",
        ));
    }
    Ok(())
}

/// Find one action by its stable id after validation.
pub fn action<'a>(descriptor: &'a Descriptor, id: &str) -> Option<&'a Action> {
    descriptor.actions.iter().find(|action| action.id == id)
}

/// Host maximums; package values can only lower these limits.
pub fn host_text_limit(field: &str) -> Option<usize> {
    match field {
        "display_name" => Some(120),
        "email" => Some(254),
        "phone" => Some(24),
        "source" => Some(40),
        _ => None,
    }
}

pub const HOST_TAG_ITEMS: usize = 16;
pub const HOST_TAG_LENGTH: usize = 40;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn manifest() -> Manifest {
        Manifest {
            app: "crm".into(),
            title: "CRM".into(),
            version: "1.0.0".into(),
            connections: vec![],
            capabilities: BTreeMap::new(),
            summary: None,
            view_contract: Some(app_view::CONTRACT.into()),
            binding_contract: Some(app_binding::CONTRACT.into()),
            action_contract: Some(CONTRACT.into()),
            guide: String::new(),
        }
    }

    fn example() -> (
        Descriptor,
        app_view::Descriptor,
        app_binding::Binding,
        Manifest,
    ) {
        let action = parse_str(include_str!(
            "../../contracts/app-actions/v2/examples/crm.json"
        ))
        .unwrap();
        let views = app_view::parse_descriptor(include_str!(
            "../../contracts/app-views/v1/examples/crm.json"
        ))
        .unwrap();
        let binding = app_binding::parse_binding(include_str!(
            "../../contracts/app-bindings/v1/examples/crm.json"
        ))
        .unwrap();
        (action, views, binding, manifest())
    }

    #[test]
    fn crm_example_is_a_closed_action_view_binding_pair() {
        let (actions, views, binding, manifest) = example();
        validate_against(&actions, &manifest, Some(&views), Some(&binding)).unwrap();
        assert_eq!(
            action(&actions, "customer.create").unwrap().form_view,
            "customer-create-form"
        );
        assert_eq!(
            action(&actions, "customer.update").unwrap().form_view,
            "customer-edit-form"
        );
    }

    #[test]
    fn form_views_are_resolved_from_the_descriptor_not_a_host_name_allowlist() {
        let mut actions_raw: Value = serde_json::from_str(include_str!(
            "../../contracts/app-actions/v2/examples/crm.json"
        ))
        .unwrap();
        actions_raw["actions"][0]["form_view"] = Value::String("signup_form".into());
        actions_raw["actions"][1]["form_view"] = Value::String("profile_editor".into());
        let actions = parse(&actions_raw).unwrap();

        let mut views_raw: Value = serde_json::from_str(include_str!(
            "../../contracts/app-views/v1/examples/crm.json"
        ))
        .unwrap();
        views_raw["views"][2]["id"] = Value::String("signup_form".into());
        views_raw["views"][3]["id"] = Value::String("profile_editor".into());
        let views_text = serde_json::to_string(&views_raw).unwrap();
        let views = app_view::parse_descriptor(&views_text).unwrap();
        let binding = app_binding::parse_binding(include_str!(
            "../../contracts/app-bindings/v1/examples/crm.json"
        ))
        .unwrap();
        validate_against(&actions, &manifest(), Some(&views), Some(&binding)).unwrap();
    }

    #[test]
    fn preview_mismatch_and_authority_fields_are_refused() {
        let mut raw: Value = serde_json::from_str(include_str!(
            "../../contracts/app-actions/v2/examples/crm.json"
        ))
        .unwrap();
        raw["actions"][1]["form_view"] = Value::String("missing-form".into());
        let actions = parse(&raw).unwrap();
        let (_, views, binding, manifest) = example();
        assert!(validate_against(&actions, &manifest, Some(&views), Some(&binding)).is_err());

        let mut raw: Value = serde_json::from_str(include_str!(
            "../../contracts/app-actions/v2/examples/crm.json"
        ))
        .unwrap();
        raw["actions"][0]["input"]["fields"][0]["expected_revision"] = serde_json::json!(1);
        assert!(parse(&raw)
            .unwrap_err()
            .to_string()
            .contains("forbidden descriptor key"));
    }

    #[test]
    fn unicode_text_bounds_count_codepoints_through_the_delegated_v1_parser() {
        let mut accented: Value = serde_json::from_str(include_str!(
            "../../contracts/app-actions/v2/examples/crm.json"
        ))
        .unwrap();
        accented["actions"][0]["input"]["fields"][0]["label"] = Value::String("é".repeat(41));
        assert!(
            parse(&accented).is_ok(),
            "41 accented codepoints fit an 80-character label"
        );

        let mut astral: Value = serde_json::from_str(include_str!(
            "../../contracts/app-actions/v2/examples/crm.json"
        ))
        .unwrap();
        astral["actions"][0]["title"] = Value::String("🧪".repeat(120));
        astral["actions"][0]["input"]["fields"][0]["label"] = Value::String("🧪".repeat(80));
        assert!(
            parse(&astral).is_ok(),
            "120/80 astral codepoints fit their metadata bounds"
        );

        astral["actions"][0]["input"]["fields"][0]["label"] = Value::String("🧪".repeat(81));
        assert!(
            parse(&astral).is_err(),
            "the delegated v1 bound still refuses 81 codepoints"
        );
    }
}
