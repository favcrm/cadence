//! App actions contract v1 (CAD-964, toward CAD-811): the strict,
//! pure parser/validator for `contracts/app-actions/v1` — a versioned,
//! data-only declaration of the record mutations a workspace app
//! package may *describe* for the trusted host.
//!
//! This module is deliberately the smallest closed slice of the
//! action contract: it decides whether untrusted bytes are a
//! well-formed `app-actions/v1` descriptor, and nothing more. It does
//! **not** install a bundle, admit a manifest key, dispatch an action,
//! or execute anything. `app.md` still refuses `actions`/
//! `needs.actions` outright (they remain gated keys), so a bundle
//! carrying this file is still refused at install — the verified-
//! receipt seam that would carry a validated descriptor is the open
//! CAD-864 slice, consumed by a later ticket.
//!
//! The grammar mirrors `app-actions.schema.json`; the two are the same
//! grammar in two notations and a change must land in both. The schema
//! expresses the closed shape (closed types, identifier grammar,
//! forbidden keys at each object's `propertyNames`); this module adds
//! what a schema cannot — unique action/field ids, enum/values
//! coherence, the recursive forbidden-key scan that runs *before*
//! shape checks, and the node/depth/byte budgets — exactly as the
//! `app-views/v1` consumer (`contract.ts`) does for view descriptors.
//!
//! Authority rule, stated once and enforced by the *shape*: the
//! package supplies metadata only. The actor is derived by the host
//! from the connection, never from bundle text; the operator-write
//! floor, context/digest checks and the update `expected_revision`
//! compare-and-set are mandatory host semantics — not optional
//! booleans a declaration toggles — so this grammar has no
//! `revision_pin`/`approval_pin`/`actor_request` field at all and
//! refuses them by name. A declaration can only ever narrow authority,
//! never widen it.

use serde_json::Value;

use crate::error::{Error, Result};

/// The one contract tag this parser accepts. Anything else — a later
/// draft's tag, a sibling contract's, a near-miss — refuses.
pub const CONTRACT: &str = "app-actions/v1";

/* ------------------------------------------------------------------ */
/* Bounds and grammar. Small — this is a review surface, not an       */
/* engine. Mirrors app-actions.schema.json.                            */
/* ------------------------------------------------------------------ */

const MAX_ACTIONS: usize = 16;
const MAX_FIELDS: usize = 64;
const MAX_ENUM_VALUES: usize = 24;
const MAX_TITLE_LEN: usize = 120;
const MAX_LABEL_LEN: usize = 80;
const MAX_SUMMARY_LEN: usize = 280;
/// Identifier segment length cap (the `0,63` of the IDENT grammar).
const MAX_IDENT_LEN: usize = 64;
/// Whole input, serialized: a bound on the bytes a caller can push
/// through `parse`/`parse_str` in one shot.
const MAX_SERIALIZED_BYTES: usize = 64 * 1024;
/// Recursion budget for the unsafe scan — shared node count and
/// nesting depth, checked before any shape read.
const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 24;

/// Field `type` vocabulary — the same closed formats the shared host
/// controls and `app-views/v1` render, so an action input is a surface
/// the host already knows how to collect. v1 keeps it deliberately
/// small; `object`/`array`/`json`/`ref`/`file`/`schema`/`any` and raw
/// JSON-schema keywords are all refused.
const FIELD_TYPES: &[&str] = &["text", "number", "date", "datetime", "enum", "tags"];

/// `operation` vocabulary: a closed, named semantic reference to a
/// host operation — never a method, URL, SQL or arbitrary verb. v1
/// covers the CRM customer-form case only; `record.delete`, `custom`,
/// `send`/`publish` and every outward effect are refused.
const OPERATIONS: &[&str] = &["record.create", "record.update"];

/// Keys a descriptor may never carry — recursively, at every level.
/// They name executable surfaces, URL/navigation escapes, scope and
/// actor identity, storage internals, authority claims, or guard
/// toggles the host alone owns. A data-only contract has no legitimate
/// use for any of them. Kept in step with
/// `app-actions.schema.json`'s `propertyNames` and with the
/// `app-views/v1` forbidden list, plus the guard-weakening keys that
/// contract does not need because views carry no mutations.
pub const FORBIDDEN_DESCRIPTOR_KEYS: &[&str] = &[
    "__proto__",
    "prototype",
    "constructor",
    "script",
    "scripts",
    "code",
    "html",
    "innerHTML",
    "css",
    "style",
    "javascript",
    "eval",
    "import",
    "module",
    "url",
    "uri",
    "href",
    "src",
    "link",
    "endpoint",
    "method",
    "route",
    "install_id",
    "installId",
    "context_id",
    "contextId",
    "workspace",
    "workspace_id",
    "project",
    "project_id",
    "project_link",
    "actor",
    "by",
    "role",
    "grant",
    "scope",
    "scopes",
    "capability",
    "capabilities",
    "caller",
    "secret",
    "secrets",
    "credential",
    "credentials",
    "token",
    "password",
    "sql",
    "query",
    "path",
    "file",
    "effect",
    "effects",
    "verified",
    "digest",
    "revision",
    // Guard-weakening toggles — forbidden so a package can never claim
    // (or waive) the host's mandatory floor. See the module docs.
    "revision_pin",
    "approval_pin",
    "actor_request",
    "guard",
    "public",
    "unauthenticated",
    "skip_approval",
    // Arbitrary constraint / schema escapes — the field grammar is
    // closed, not a JSON-Schema subset.
    "default",
    "pattern",
    "regex",
    "expression",
    "formula",
    "schema",
    "properties",
    "items",
    "send",
];

/* ------------------------------------------------------------------ */
/* Public types — the validated v1 shape.                              */
/* ------------------------------------------------------------------ */

/// The closed operation a declared action names. v1 has exactly two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    RecordCreate,
    RecordUpdate,
}

/// One closed typed input field an action's host form may collect.
/// `values` is `Some` only for `enum`; `max_length`/`max_items` are
/// declared caps the host may honor but never exceeds its own floor.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub id: String,
    pub label: String,
    /// One of [`FIELD_TYPES`]; absent in JSON is `"text"`.
    pub field_type: String,
    pub required: bool,
    /// Closed allowlist — `Some` iff `field_type == "enum"`.
    pub values: Option<Vec<String>>,
    pub max_length: Option<u32>,
    pub max_items: Option<u32>,
}

/// The closed input block of one action: a unique set of declared
/// fields. No `default`/`pattern`/expression surface exists.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub fields: Vec<Field>,
}

/// One declared action: a unique id, a closed operation, a record-kind
/// reference and a closed input. All metadata — the host decides if it
/// is ever honored.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub operation: Operation,
    /// A record-kind name (identifier grammar) the package declares
    /// elsewhere. Well-formedness is checked; existence is not — that
    /// is a future `domain/` admission requirement.
    pub record: String,
    pub input: Input,
}

/// A validated `app-actions/v1` descriptor: provenance `app`, a title,
/// and a non-empty set of uniquely-id'd actions.
#[derive(Clone, Debug, PartialEq)]
pub struct Descriptor {
    pub app: String,
    pub title: String,
    pub summary: Option<String>,
    pub actions: Vec<Action>,
}

/* ------------------------------------------------------------------ */
/* Small checked readers.                                              */
/* ------------------------------------------------------------------ */

fn fail(path: &str, message: impl std::fmt::Display) -> Error {
    Error::rejected(format!("app-actions/v1 {path}: {message}"))
}

fn is_plain(v: &Value) -> bool {
    // serde_json has exactly six variants; Null/Bool/String/Number are
    // leaves. `arbitrary_precision` is not enabled, so a Number is always
    // finite and inert — no NaN/Infinity value exists to check.
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn is_ident(s: &str) -> bool {
    // One identifier segment: [a-z][a-z0-9_-]{0,63}.
    !s.is_empty()
        && s.len() <= MAX_IDENT_LEN
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn is_action_id(s: &str) -> bool {
    // Up to four dotted identifier segments.
    !s.is_empty() && s.split('.').all(is_ident) && s.split('.').count() <= 4
}

fn has_control(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
}

fn text<'a>(v: &'a Value, path: &str, max: usize) -> Result<&'a str> {
    let Value::String(s) = v else {
        return Err(fail(path, "expected a non-empty string"));
    };
    if s.is_empty() {
        return Err(fail(path, "expected a non-empty string"));
    }
    if s.chars().count() > max {
        return Err(fail(
            path,
            format!("string is longer than {max} characters"),
        ));
    }
    if has_control(s) {
        return Err(fail(path, "control characters are not allowed"));
    }
    Ok(s)
}

fn ident<'a>(v: &'a Value, path: &str) -> Result<&'a str> {
    let s = text(v, path, MAX_IDENT_LEN)?;
    if !is_ident(s) {
        return Err(fail(path, format!("unsafe identifier: {s:?}")));
    }
    Ok(s)
}

/// Deep-scan `value` for forbidden keys and oversized structures.
/// Runs before shape validation so a hostile payload cannot push the
/// shape checks through a huge or nested shell. Arrays and objects
/// both count toward one shared node budget.
fn scan_unsafe(v: &Value, path: &str, budget: &mut usize, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(fail(path, "input is nested too deeply"));
    }
    *budget += 1;
    if *budget > MAX_NODES {
        return Err(fail(path, "input has too many nodes"));
    }
    if is_plain(v) {
        return Ok(());
    }
    match v {
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                scan_unsafe(item, &format!("{path}.{i}"), budget, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (key, property) in map {
                if FORBIDDEN_DESCRIPTOR_KEYS.contains(&key.as_str()) {
                    return Err(fail(path, format!("forbidden descriptor key: {key}")));
                }
                scan_unsafe(property, &format!("{path}.{key}"), budget, depth + 1)?;
            }
            Ok(())
        }
        // `is_plain` already covered Null/Bool/String/Number; only a
        // non-finite or otherwise-unreachable leaf can land here.
        _ => Err(fail(path, "expected plain JSON data")),
    }
}

fn bounded_json(v: &Value, path: &str) -> Result<()> {
    // Scan before serialization: cycles and oversized structures must
    // refuse without overflowing the stack.
    let mut budget = 0usize;
    scan_unsafe(v, path, &mut budget, 0)?;
    let bytes = serde_json::to_vec(v)
        .map_err(|e| fail(path, format!("not serializable: {e}")))?
        .len();
    if bytes > MAX_SERIALIZED_BYTES {
        return Err(fail(
            path,
            format!("input exceeds {MAX_SERIALIZED_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// Refuse any key outside `allowed` on an object — the closed-shape
/// check that keeps a package from smuggling a declaration past review.
fn closed_keys(map: &serde_json::Map<String, Value>, allowed: &[&str], path: &str) -> Result<()> {
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(fail(path, format!("unknown key: {key}")));
        }
    }
    Ok(())
}

fn parse_field(raw: &Value, path: &str) -> Result<Field> {
    let Value::Object(map) = raw else {
        return Err(fail(path, "field must be an object"));
    };
    closed_keys(
        map,
        &[
            "id",
            "label",
            "type",
            "required",
            "values",
            "maxLength",
            "maxItems",
        ],
        path,
    )?;
    let id = ident(map.get("id").unwrap_or(&Value::Null), &format!("{path}.id"))?.to_string();
    let label = text(
        map.get("label").unwrap_or(&Value::Null),
        &format!("{path}.label"),
        MAX_LABEL_LEN,
    )?
    .to_string();
    let field_type = match map.get("type") {
        None => "text".to_string(),
        Some(v) => {
            let t = text(v, &format!("{path}.type"), MAX_IDENT_LEN)?;
            if !FIELD_TYPES.contains(&t) {
                return Err(fail(
                    &format!("{path}.type"),
                    format!("expected one of {}", FIELD_TYPES.join(", ")),
                ));
            }
            t.to_string()
        }
    };
    let required = match map.get("required") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(fail(&format!("{path}.required"), "expected a boolean")),
    };
    let values = match map.get("values") {
        None => None,
        Some(Value::Array(items)) => {
            if field_type != "enum" {
                return Err(fail(
                    &format!("{path}.values"),
                    "values is only allowed on type \"enum\"",
                ));
            }
            if items.is_empty() || items.len() > MAX_ENUM_VALUES {
                return Err(fail(
                    &format!("{path}.values"),
                    format!("enum needs 1–{MAX_ENUM_VALUES} values"),
                ));
            }
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                out.push(text(item, &format!("{path}.values.{i}"), MAX_LABEL_LEN)?.to_string());
            }
            Some(out)
        }
        Some(_) => return Err(fail(&format!("{path}.values"), "expected an array")),
    };
    if field_type == "enum" && values.is_none() {
        return Err(fail(
            &format!("{path}.type"),
            "type \"enum\" requires a values list",
        ));
    }
    let bounded = |key: &str, cap: u32| -> Result<Option<u32>> {
        match map.get(key) {
            None => Ok(None),
            Some(v) => {
                let n = v
                    .as_u64()
                    .ok_or_else(|| fail(&format!("{path}.{key}"), "expected a positive integer"))?;
                if n == 0 || n > cap as u64 {
                    return Err(fail(&format!("{path}.{key}"), format!("expected 1–{cap}")));
                }
                Ok(Some(n as u32))
            }
        }
    };
    Ok(Field {
        id,
        label,
        field_type,
        required,
        values,
        max_length: bounded("maxLength", 4096)?,
        max_items: bounded("maxItems", 64)?,
    })
}

fn parse_input(raw: &Value, path: &str) -> Result<Input> {
    let Value::Object(map) = raw else {
        return Err(fail(path, "input must be an object"));
    };
    closed_keys(map, &["fields"], path)?;
    let Value::Array(items) = map.get("fields").unwrap_or(&Value::Null) else {
        return Err(fail(&format!("{path}.fields"), "expected an array"));
    };
    if items.is_empty() || items.len() > MAX_FIELDS {
        return Err(fail(
            &format!("{path}.fields"),
            format!("expected 1–{MAX_FIELDS} fields"),
        ));
    }
    let mut fields = Vec::with_capacity(items.len());
    let mut seen = std::collections::HashSet::new();
    for (i, item) in items.iter().enumerate() {
        let field = parse_field(item, &format!("{path}.fields.{i}"))?;
        if !seen.insert(field.id.clone()) {
            return Err(fail(
                &format!("{path}.fields"),
                format!("duplicate field id: {}", field.id),
            ));
        }
        fields.push(field);
    }
    Ok(Input { fields })
}

fn parse_action(raw: &Value, path: &str) -> Result<Action> {
    let Value::Object(map) = raw else {
        return Err(fail(path, "action must be an object"));
    };
    closed_keys(map, &["id", "title", "operation", "record", "input"], path)?;
    let id_v = map.get("id").unwrap_or(&Value::Null);
    let id = text(id_v, &format!("{path}.id"), MAX_IDENT_LEN * 4 + 3)?.to_string();
    if !is_action_id(&id) {
        return Err(fail(
            &format!("{path}.id"),
            format!("unsafe action id: {id:?}"),
        ));
    }
    let title = text(
        map.get("title").unwrap_or(&Value::Null),
        &format!("{path}.title"),
        MAX_TITLE_LEN,
    )?
    .to_string();
    let operation = match map.get("operation") {
        Some(Value::String(op)) if OPERATIONS.contains(&op.as_str()) => match op.as_str() {
            "record.create" => Operation::RecordCreate,
            _ => Operation::RecordUpdate,
        },
        _ => {
            return Err(fail(
                &format!("{path}.operation"),
                format!("expected one of {}", OPERATIONS.join(", ")),
            ))
        }
    };
    let record = ident(
        map.get("record").unwrap_or(&Value::Null),
        &format!("{path}.record"),
    )?
    .to_string();
    let input = parse_input(
        map.get("input").unwrap_or(&Value::Null),
        &format!("{path}.input"),
    )?;
    Ok(Action {
        id,
        title,
        operation,
        record,
        input,
    })
}

/// Validate `raw` as an `app-actions/v1` descriptor. Fails closed on
/// the first violation — unknown keys, forbidden keys anywhere in the
/// tree, bad identifier grammar, oversized strings/arrays, duplicate
/// ids, wrong or missing contract tag. On success returns a fresh,
/// structurally-owned `Descriptor`.
///
/// Data only: a `Ok` result means "well-formed declaration," never
/// "permitted," "installed" or "executable."
pub fn parse(raw: &Value) -> Result<Descriptor> {
    bounded_json(raw, "$")?;

    let Value::Object(map) = raw else {
        return Err(fail("$", "descriptor must be an object"));
    };
    closed_keys(
        map,
        &["contract", "app", "title", "summary", "actions"],
        "$",
    )?;
    match map.get("contract") {
        Some(Value::String(c)) if c == CONTRACT => {}
        _ => return Err(fail("$.contract", format!("expected {CONTRACT:?}"))),
    }
    let app = ident(map.get("app").unwrap_or(&Value::Null), "$.app")?.to_string();
    let title = text(
        map.get("title").unwrap_or(&Value::Null),
        "$.title",
        MAX_TITLE_LEN,
    )?
    .to_string();
    let summary = match map.get("summary") {
        None => None,
        Some(v) => Some(text(v, "$.summary", MAX_SUMMARY_LEN)?.to_string()),
    };
    let Value::Array(items) = map.get("actions").unwrap_or(&Value::Null) else {
        return Err(fail("$.actions", "expected an array"));
    };
    if items.is_empty() || items.len() > MAX_ACTIONS {
        return Err(fail(
            "$.actions",
            format!("expected 1–{MAX_ACTIONS} actions"),
        ));
    }
    let mut actions = Vec::with_capacity(items.len());
    let mut seen = std::collections::HashSet::new();
    for (i, item) in items.iter().enumerate() {
        let action = parse_action(item, &format!("$.actions.{i}"))?;
        if !seen.insert(action.id.clone()) {
            return Err(fail(
                "$.actions",
                format!("duplicate action id: {}", action.id),
            ));
        }
        actions.push(action);
    }
    Ok(Descriptor {
        app,
        title,
        summary,
        actions,
    })
}

/// Parse descriptor bytes: a JSON text is decoded, then [`parse`]
/// validates it. Both source bytes (before decoding) and the decoded
/// serialized value are bounded, including excessive JSON whitespace.
pub fn parse_str(text: &str) -> Result<Descriptor> {
    if text.len() > MAX_SERIALIZED_BYTES {
        return Err(fail(
            "$",
            format!("input exceeds {MAX_SERIALIZED_BYTES} bytes"),
        ));
    }
    let raw: Value =
        serde_json::from_str(text).map_err(|e| fail("$", format!("not valid JSON: {e}")))?;
    parse(&raw)
}
