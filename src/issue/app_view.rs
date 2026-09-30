//! `app-views/v1` descriptor validation (CAD-864, toward CAD-811).
//!
//! A package may ship one data-only view descriptor at
//! `views/app-views-v1.json`, declared by `needs.views.contract` in
//! `app.md`. This module is the Rust gate the install/upgrade paths run
//! before those bytes are trusted: it mirrors the grammar and bounds of
//! `contracts/app-views/v1/app-view.schema.json` and the consumer rules
//! in `ui/src/features/app-shell/app-views/contract.ts`, fail-closed.
//! The validated [`Descriptor`] is what a verified installation receipt
//! carries — descriptor bytes never name an installation, actor, scope
//! or URL, so the receipt's catalog-bound `install_id` remains the only
//! authority.

use serde_json::Value;
use std::collections::BTreeSet;

use crate::error::{Error, Result};

/// The contract tag — the filename and this string pin the version.
pub const CONTRACT: &str = "app-views/v1";

/// The one file a bundle may carry under `views/`.
pub const FILE: &str = "app-views-v1.json";

/// The bundle-relative path `bundle_files`/`snapshot` produce.
pub const REL_PATH: &str = "views/app-views-v1.json";

/* Bounds mirroring contract.ts — keep them identical. */
const MAX_VIEWS: usize = 16;
const MAX_FIELDS_PER_VIEW: usize = 24;
const MAX_COLUMNS_PER_VIEW: usize = 12;
const MAX_ENUM_VALUES: usize = 24;
const MAX_ID_LENGTH: usize = 64;
const MAX_LABEL_LENGTH: usize = 80;
const MAX_TITLE_LENGTH: usize = 120;
const MAX_SUMMARY_LENGTH: usize = 280;
const MAX_SERIALIZED_BYTES: usize = 64 * 1024;
const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 24;

const FORMATS: &[&str] = &["text", "number", "date", "datetime", "enum", "tags"];
const KINDS: &[&str] = &["scalar", "list"];
const VIEW_KINDS: &[&str] = &["table", "detail", "form"];
const FIELD_KEYS: &[&str] = &["id", "label", "format", "kind", "values", "createView"];
const VIEW_KEYS: &[&str] = &["id", "title", "kind", "fields", "columns", "previewOf"];
const DESCRIPTOR_KEYS: &[&str] = &["contract", "app", "title", "summary", "views"];
const COLUMN_KEYS: &[&str] = &["field", "label"];

/// Keys a descriptor may never carry, recursively at every level —
/// the same list `FORBIDDEN_DESCRIPTOR_KEYS` in contract.ts enforces.
/// They name executable surfaces, URL/navigation escapes, scope/actor
/// identity, credentials, storage internals or authority the host owns.
const FORBIDDEN_DESCRIPTOR_KEYS: &[&str] = &[
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
    "action",
    "endpoint",
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
];

fn fail(path: &str, message: impl Into<String>) -> Error {
    Error::rejected(format!("{path}: {}", message.into()))
}

/// `contract.ts`'s CONTROL: C0/C1 controls, DEL and the JS line
/// separators — descriptor strings stay single-line plain text.
fn has_control(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{2028}' | '\u{2029}'))
}

/// Identifier grammar `[a-z][a-z0-9_-]{0,63}` — field ids, view ids and
/// the descriptor's `app` provenance share it.
fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LENGTH
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

fn expect_string<'a>(v: &'a Value, path: &str, max: usize) -> Result<&'a str> {
    let Some(s) = v.as_str() else {
        return Err(fail(path, "expected a non-empty string"));
    };
    if s.is_empty() {
        return Err(fail(path, "expected a non-empty string"));
    }
    // Parity with contract.ts `v.length`: JavaScript counts UTF-16 code
    // units — an astral character is two — so Rust must not count
    // `chars()` (Unicode scalars) or the gate admits what the consumer
    // refuses.
    if s.encode_utf16().count() > max {
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

fn expect_ident<'a>(v: &'a Value, path: &str) -> Result<&'a str> {
    let s = expect_string(v, path, MAX_ID_LENGTH)?;
    if !is_ident(s) {
        return Err(fail(path, format!("unsafe identifier: {s:?}")));
    }
    Ok(s)
}

fn expect_object<'a>(v: &'a Value, path: &str) -> Result<&'a serde_json::Map<String, Value>> {
    v.as_object()
        .ok_or_else(|| fail(path, "expected a JSON object"))
}

fn only_keys(map: &serde_json::Map<String, Value>, allowed: &[&str], path: &str) -> Result<()> {
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(fail(path, format!("unknown key: {key}")));
        }
    }
    Ok(())
}

fn expect_array<'a>(v: &'a Value, path: &str) -> Result<&'a [Value]> {
    v.as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| fail(path, "expected an array"))
}

/// Recursive hostile-input scan — runs over the raw `Value` before any
/// shape check, so a forbidden key or a deep/wide bomb refuses with the
/// same error wherever it sits. `serde_json` already produced a tree of
/// plain objects/arrays/scalars (no functions, no cycles, no
/// non-finite floats past `arbitrary_precision`); this enforces the
/// contract's node/depth budget and forbidden keys.
fn scan_unsafe(v: &Value, path: &str, nodes: &mut usize, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(fail(path, "input is nested too deeply"));
    }
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err(fail(path, "input has too many nodes"));
    }
    match v {
        Value::Object(map) => {
            for (key, value) in map {
                if FORBIDDEN_DESCRIPTOR_KEYS.contains(&key.as_str()) {
                    return Err(fail(path, format!("forbidden descriptor key: {key}")));
                }
                scan_unsafe(value, &format!("{path}.{key}"), nodes, depth + 1)?;
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                scan_unsafe(item, &format!("{path}.{i}"), nodes, depth + 1)?;
            }
        }
        Value::Number(n) => {
            // serde_json never produces NaN/Infinity; reject anyway so
            // the guard does not depend on parser internals.
            if !n.is_f64() && !n.is_i64() && !n.is_u64() {
                return Err(fail(path, "non-finite JSON numbers refuse"));
            }
            if n.as_f64().is_some_and(|f| !f.is_finite()) {
                return Err(fail(path, "non-finite JSON numbers refuse"));
            }
        }
        Value::String(_) | Value::Bool(_) | Value::Null => {}
    }
    Ok(())
}

fn parse_string_list(
    v: &Value,
    path: &str,
    max_items: usize,
    max_len: usize,
) -> Result<Vec<String>> {
    let items = expect_array(v, path)?;
    if items.len() > max_items {
        return Err(fail(path, format!("more than {max_items} entries")));
    }
    items
        .iter()
        .enumerate()
        .map(|(i, item)| expect_string(item, &format!("{path}.{i}"), max_len).map(str::to_string))
        .collect()
}

/// One validated descriptor field (`fields[]` / `previewOf[]` entry).
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub id: String,
    pub label: String,
    pub format: String,
    pub kind: String,
    pub values: Option<Vec<String>>,
    pub create_view: Option<String>,
}

fn parse_field(v: &Value, path: &str) -> Result<Field> {
    let map = expect_object(v, path)?;
    only_keys(map, FIELD_KEYS, path)?;
    let id = expect_ident(&v["id"], &format!("{path}.id"))?.to_string();
    let label = expect_string(&v["label"], &format!("{path}.label"), MAX_LABEL_LENGTH)?.to_string();
    let format = match map.get("format") {
        None => "text".to_string(),
        Some(f) => {
            let f = expect_string(f, &format!("{path}.format"), MAX_LABEL_LENGTH)?;
            if !FORMATS.contains(&f) {
                return Err(fail(
                    &format!("{path}.format"),
                    format!("expected one of {}", FORMATS.join(", ")),
                ));
            }
            f.to_string()
        }
    };
    let kind = match map.get("kind") {
        None => "scalar".to_string(),
        Some(k) => {
            let k = expect_string(k, &format!("{path}.kind"), MAX_LABEL_LENGTH)?;
            if !KINDS.contains(&k) {
                return Err(fail(
                    &format!("{path}.kind"),
                    format!("expected one of {}", KINDS.join(", ")),
                ));
            }
            k.to_string()
        }
    };
    let values = match map.get("values") {
        None => None,
        Some(raw) => {
            if format != "enum" {
                return Err(fail(
                    &format!("{path}.values"),
                    "values is only allowed on format \"enum\"",
                ));
            }
            let values = parse_string_list(
                raw,
                &format!("{path}.values"),
                MAX_ENUM_VALUES,
                MAX_LABEL_LENGTH,
            )?;
            if values.is_empty() {
                return Err(fail(
                    &format!("{path}.values"),
                    "enum needs at least one value",
                ));
            }
            Some(values)
        }
    };
    if format == "enum" && values.is_none() {
        return Err(fail(
            &format!("{path}.format"),
            "format \"enum\" requires a values list",
        ));
    }
    if format == "tags" && kind != "list" {
        return Err(fail(
            &format!("{path}.kind"),
            "format \"tags\" must be kind \"list\"",
        ));
    }
    if kind == "list" && format != "text" && format != "tags" {
        return Err(fail(
            &format!("{path}.kind"),
            "v1 lists support only text or tags",
        ));
    }
    let create_view = match map.get("createView") {
        None => None,
        Some(raw) => Some(expect_ident(raw, &format!("{path}.createView"))?.to_string()),
    };
    Ok(Field {
        id,
        label,
        format,
        kind,
        values,
        create_view,
    })
}

/// One validated column (`table` view only).
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub field: String,
    pub label: Option<String>,
}

/// One validated view.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub id: String,
    pub title: String,
    pub kind: String,
    /// table/detail only.
    pub fields: Vec<Field>,
    /// table only.
    pub columns: Vec<Column>,
    /// form only.
    pub preview_of: Vec<Field>,
}

fn parse_view(v: &Value, path: &str) -> Result<View> {
    let map = expect_object(v, path)?;
    only_keys(map, VIEW_KEYS, path)?;
    let id = expect_ident(&v["id"], &format!("{path}.id"))?.to_string();
    let title = expect_string(&v["title"], &format!("{path}.title"), MAX_TITLE_LENGTH)?.to_string();
    let kind_raw = expect_string(&v["kind"], &format!("{path}.kind"), MAX_LABEL_LENGTH)?;
    if !VIEW_KINDS.contains(&kind_raw) {
        return Err(fail(
            &format!("{path}.kind"),
            format!("expected one of {}", VIEW_KINDS.join(", ")),
        ));
    }
    let kind = kind_raw.to_string();

    if kind == "form" {
        if map.contains_key("fields") || map.contains_key("columns") {
            return Err(fail(
                path,
                "a \"form\" view carries previewOf, not fields/columns",
            ));
        }
        let raw = map.get("previewOf").ok_or_else(|| {
            fail(
                &format!("{path}.previewOf"),
                "a \"form\" view needs at least one preview field",
            )
        })?;
        let items = expect_array(raw, &format!("{path}.previewOf"))?;
        if items.is_empty() {
            return Err(fail(
                &format!("{path}.previewOf"),
                "a \"form\" view needs at least one preview field",
            ));
        }
        if items.len() > MAX_FIELDS_PER_VIEW {
            return Err(fail(
                &format!("{path}.previewOf"),
                format!("more than {MAX_FIELDS_PER_VIEW} fields"),
            ));
        }
        let mut preview_of = Vec::with_capacity(items.len());
        let mut seen = BTreeSet::new();
        for (i, item) in items.iter().enumerate() {
            let field = parse_field(item, &format!("{path}.previewOf.{i}"))?;
            if !seen.insert(field.id.clone()) {
                return Err(fail(
                    &format!("{path}.previewOf"),
                    format!("duplicate field id: {}", field.id),
                ));
            }
            preview_of.push(field);
        }
        return Ok(View {
            id,
            title,
            kind,
            fields: Vec::new(),
            columns: Vec::new(),
            preview_of,
        });
    }

    if map.contains_key("previewOf") {
        return Err(fail(
            path,
            format!("a \"{kind}\" view cannot carry previewOf"),
        ));
    }
    let raw_fields = map.get("fields").ok_or_else(|| {
        fail(
            &format!("{path}.fields"),
            format!("a \"{kind}\" view needs at least one declared field"),
        )
    })?;
    let items = expect_array(raw_fields, &format!("{path}.fields"))?;
    if items.is_empty() {
        return Err(fail(
            &format!("{path}.fields"),
            format!("a \"{kind}\" view needs at least one declared field"),
        ));
    }
    if items.len() > MAX_FIELDS_PER_VIEW {
        return Err(fail(
            &format!("{path}.fields"),
            format!("more than {MAX_FIELDS_PER_VIEW} fields"),
        ));
    }
    let mut fields = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
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

    let mut columns = Vec::new();
    if kind == "table" {
        let raw = map.get("columns").ok_or_else(|| {
            fail(
                &format!("{path}.columns"),
                "a \"table\" view needs at least one column",
            )
        })?;
        let items = expect_array(raw, &format!("{path}.columns"))?;
        if items.is_empty() {
            return Err(fail(
                &format!("{path}.columns"),
                "a \"table\" view needs at least one column",
            ));
        }
        if items.len() > MAX_COLUMNS_PER_VIEW {
            return Err(fail(
                &format!("{path}.columns"),
                format!("more than {MAX_COLUMNS_PER_VIEW} columns"),
            ));
        }
        let declared: BTreeSet<&str> = fields.iter().map(|f| f.id.as_str()).collect();
        let mut used = BTreeSet::new();
        for (i, item) in items.iter().enumerate() {
            let cp = format!("{path}.columns.{i}");
            let map = expect_object(item, &cp)?;
            only_keys(map, COLUMN_KEYS, &cp)?;
            let field = expect_ident(&item["field"], &format!("{cp}.field"))?;
            if !declared.contains(field) {
                return Err(fail(
                    &format!("{cp}.field"),
                    format!("column names undeclared field: {field}"),
                ));
            }
            if !used.insert(field.to_string()) {
                return Err(fail(
                    &format!("{cp}.field"),
                    format!("duplicate column field: {field}"),
                ));
            }
            let label = match map.get("label") {
                None => None,
                Some(l) => {
                    Some(expect_string(l, &format!("{cp}.label"), MAX_LABEL_LENGTH)?.to_string())
                }
            };
            columns.push(Column {
                field: field.to_string(),
                label,
            });
        }
    } else if map.contains_key("columns") {
        return Err(fail(
            path,
            format!("a \"{kind}\" view cannot carry columns"),
        ));
    }

    Ok(View {
        id,
        title,
        kind,
        fields,
        columns,
        preview_of: Vec::new(),
    })
}

/// The validated descriptor. The `serde_json::Value` it was parsed
/// from is retained for the receipt — the descriptor *is* data, so the
/// receipt serves the exact reviewed bytes (as a parsed value), not a
/// re-rendered struct.
#[derive(Clone, Debug)]
pub struct Descriptor {
    pub app: String,
    pub title: String,
    pub summary: Option<String>,
    pub views: Vec<View>,
    /// The validated JSON value — what the installation receipt serves.
    pub raw: Value,
}

/// Validate `text` as an `app-views/v1` descriptor. Fails closed on the
/// first violation: not-JSON, wrong contract tag, unknown keys,
/// forbidden keys anywhere in the tree, bad identifier grammar,
/// oversized strings/arrays, undeclared column or createView
/// references, node/depth/byte bounds.
pub fn parse_descriptor(text: &str) -> Result<Descriptor> {
    // The package-side bound counts the raw descriptor FILE bytes
    // (`text.len()`), not the normalized re-serialization contract.ts
    // measures with `JSON.stringify(raw)` — the two differ (whitespace,
    // escape forms, key order all survive in file bytes). This gate is
    // deliberately the stricter envelope: file bytes ≤64 KiB implies
    // the parsed tree is at most as large, and the consumer's own cap
    // re-measures the normalized form on its side. Documented as
    // separate caps, not identical semantics.
    if text.len() > MAX_SERIALIZED_BYTES {
        return Err(fail(
            "$",
            format!("input exceeds {MAX_SERIALIZED_BYTES} bytes"),
        ));
    }
    let raw: Value = serde_json::from_str(text)
        .map_err(|e| fail("$", format!("descriptor is not JSON: {e}")))?;
    let mut nodes = 0usize;
    scan_unsafe(&raw, "$", &mut nodes, 0)?;
    let map = expect_object(&raw, "$")?;
    only_keys(map, DESCRIPTOR_KEYS, "$")?;
    if raw["contract"] != Value::String(CONTRACT.to_string()) {
        return Err(fail("$.contract", format!("expected {CONTRACT:?}")));
    }
    let app = expect_ident(&raw["app"], "$.app")?.to_string();
    let title = expect_string(&raw["title"], "$.title", MAX_TITLE_LENGTH)?.to_string();
    // `summary` is optional but never null — contract.ts's `text()`
    // accepts only a present string; a JSON null is a type violation,
    // not an absence.
    let summary = match map.get("summary") {
        None => None,
        Some(s) => Some(expect_string(s, "$.summary", MAX_SUMMARY_LENGTH)?.to_string()),
    };
    let raw_views = map
        .get("views")
        .ok_or_else(|| fail("$.views", "descriptor needs at least one view"))?;
    let items = expect_array(raw_views, "$.views")?;
    if items.is_empty() {
        return Err(fail("$.views", "descriptor needs at least one view"));
    }
    if items.len() > MAX_VIEWS {
        return Err(fail("$.views", format!("more than {MAX_VIEWS} views")));
    }
    let mut views = Vec::with_capacity(items.len());
    let mut view_ids = BTreeSet::new();
    for (i, item) in items.iter().enumerate() {
        let view = parse_view(item, &format!("$.views.{i}"))?;
        if !view_ids.insert(view.id.clone()) {
            return Err(fail("$.views", format!("duplicate view id: {}", view.id)));
        }
        views.push(view);
    }
    // Cross-references: `createView` names a declared form view only.
    let form_ids: BTreeSet<&str> = views
        .iter()
        .filter(|v| v.kind == "form")
        .map(|v| v.id.as_str())
        .collect();
    let check_create_view = |fields: &[Field], path: &str| -> Result<()> {
        for f in fields {
            if let Some(cv) = &f.create_view {
                if !form_ids.contains(cv.as_str()) {
                    return Err(fail(
                        path,
                        format!("createView names no declared form view: {cv}"),
                    ));
                }
            }
        }
        Ok(())
    };
    for (i, view) in views.iter().enumerate() {
        check_create_view(&view.fields, &format!("$.views.{i}.fields"))?;
        check_create_view(&view.preview_of, &format!("$.views.{i}.previewOf"))?;
    }
    Ok(Descriptor {
        app,
        title,
        summary,
        views,
        raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> &'static str {
        r#"{"contract":"app-views/v1","app":"demo","title":"Demo","views":[
            {"id":"t","title":"T","kind":"table","fields":[{"id":"f","label":"F"}],"columns":[{"field":"f"}]},
            {"id":"d","title":"D","kind":"detail","fields":[{"id":"f","label":"F","createView":"form"}]},
            {"id":"form","title":"F","kind":"form","previewOf":[{"id":"f","label":"F"}]}
        ]}"#
    }

    #[test]
    fn parses_the_valid_descriptor() {
        let d = parse_descriptor(good()).unwrap();
        assert_eq!(d.app, "demo");
        assert_eq!(d.views.len(), 3);
        assert_eq!(d.views[0].columns[0].field, "f");
        assert_eq!(d.views[1].fields[0].create_view.as_deref(), Some("form"));
        assert_eq!(d.views[2].preview_of.len(), 1);
    }

    #[test]
    fn refuses_each_violation() {
        let long_title = format!(
            r#"{{"contract":"app-views/v1","app":"d","title":"{}","views":[{{"id":"v","title":"t","kind":"detail","fields":[{{"id":"f","label":"l"}}]}}]}}"#,
            "x".repeat(121)
        );
        let cases: Vec<(&str, &str)> = vec![
            // contract tag / envelope
            ("{}", "contract"),
            (
                r#"{"contract":"app-views/v2","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l"}]}]}"#,
                "contract",
            ),
            // unknown keys
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","extra":1,"views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l"}]}]}"#,
                "unknown key",
            ),
            // forbidden keys recursive
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","install_id":"x","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l"}]}]}"#,
                "forbidden",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l","url":"https://x"}]}]}"#,
                "forbidden",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","script":"x","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l"}]}]}"#,
                "forbidden",
            ),
            // shape violations
            (
                r#"{"contract":"app-views/v1","app":"CRM","title":"t","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l"}],"columns":[{"field":"f"}]}]}"#,
                "identifier",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[]}"#,
                "at least one view",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l"}],"columns":[{"field":"ghost"}]}]}"#,
                "undeclared field",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l"}],"columns":[{"field":"f"},{"field":"f"}]}]}"#,
                "duplicate column",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l"}],"columns":[{"field":"f"}]}]}"#,
                "cannot carry columns",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"form","fields":[{"id":"f","label":"l"}]}]}"#,
                "not fields/columns",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l","format":"enum"}],"columns":[{"field":"f"}]}]}"#,
                "requires a values list",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l","format":"text","values":["x"]}],"columns":[{"field":"f"}]}]}"#,
                "only allowed on format",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l","format":"tags"}],"columns":[{"field":"f"}]}]}"#,
                "must be kind",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"table","fields":[{"id":"f","label":"l","format":"number","kind":"list"}],"columns":[{"field":"f"}]}]}"#,
                "lists support only",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l","createView":"nope"}]}]}"#,
                "no declared form view",
            ),
            // duplicate ids
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"t","kind":"detail","fields":[{"id":"f","label":"l"},{"id":"f","label":"x"}]}]}"#,
                "duplicate field id",
            ),
            (
                r#"{"contract":"app-views/v1","app":"d","title":"t","views":[{"id":"v","title":"a","kind":"detail","fields":[{"id":"f","label":"l"}]},{"id":"v","title":"b","kind":"detail","fields":[{"id":"f","label":"l"}]}]}"#,
                "duplicate view id",
            ),
            // malformed JSON and bounds
            ("not json", "not JSON"),
            (long_title.as_str(), "longer than 120"),
        ];
        for (input, why) in cases {
            assert!(parse_descriptor(input).is_err(), "accepted {why}: {input}");
        }
    }

    #[test]
    fn forbidden_keys_all_refuse_at_descriptor_root_and_nested() {
        for key in FORBIDDEN_DESCRIPTOR_KEYS {
            let bad = format!(
                r#"{{"contract":"app-views/v1","app":"d","title":"t","{key}":"x","views":[{{"id":"v","title":"t","kind":"detail","fields":[{{"id":"f","label":"l"}}]}}]}}"#
            );
            assert!(
                parse_descriptor(&bad).is_err(),
                "forbidden key {key} accepted at root"
            );
            let nested = format!(
                r#"{{"contract":"app-views/v1","app":"d","title":"t","views":[{{"id":"v","title":"t","kind":"detail","fields":[{{"id":"f","label":"l","{key}":"x"}}]}}]}}"#
            );
            assert!(
                parse_descriptor(&nested).is_err(),
                "forbidden key {key} accepted nested"
            );
        }
    }

    #[test]
    fn over_16_views_and_over_24_fields_refuse() {
        let views: Vec<String> = (0..17)
            .map(|i| {
                format!(
                    r#"{{"id":"v{i}","title":"t","kind":"detail","fields":[{{"id":"f","label":"l"}}]}}"#
                )
            })
            .collect();
        let bad = format!(
            r#"{{"contract":"app-views/v1","app":"d","title":"t","views":[{}]}}"#,
            views.join(",")
        );
        assert!(parse_descriptor(&bad).is_err());
        let fields: Vec<String> = (0..25)
            .map(|i| format!(r#"{{"id":"f{i}","label":"l"}}"#))
            .collect();
        let bad = format!(
            r#"{{"contract":"app-views/v1","app":"d","title":"t","views":[{{"id":"v","title":"t","kind":"detail","fields":[{}]}}]}}"#,
            fields.join(",")
        );
        assert!(parse_descriptor(&bad).is_err());
    }

    /// contract.ts bounds strings by JavaScript `v.length` — UTF-16
    /// code units. An astral emoji is ONE Rust `char` but TWO UTF-16
    /// units; a Rust gate counting scalars would admit a descriptor
    /// the consumer refuses. These boundaries pin the parity.
    #[test]
    fn string_bounds_count_utf16_units_like_the_ts_consumer() {
        // 🚀 is U+1F680 — 1 char, 2 UTF-16 units.
        let rocket = "\u{1F680}";
        assert_eq!(rocket.chars().count(), 1);
        assert_eq!(rocket.encode_utf16().count(), 2);
        // title ≤120 UTF-16 units: 60 rockets == 120 (accept),
        // 61 == 122 (refuse even though chars().count() is 61).
        let view = "{\"id\":\"v\",\"title\":\"t\",\"kind\":\"detail\",\"fields\":[{\"id\":\"f\",\"label\":\"l\"}]}";
        let ok = format!(
            "{{\"contract\":\"app-views/v1\",\"app\":\"d\",\"title\":\"{}\",\"views\":[{}]}}",
            rocket.repeat(60),
            view
        );
        assert!(
            parse_descriptor(&ok).is_ok(),
            "120 UTF-16-unit title refused"
        );
        let over = format!(
            "{{\"contract\":\"app-views/v1\",\"app\":\"d\",\"title\":\"{}\",\"views\":[{}]}}",
            rocket.repeat(61),
            view
        );
        assert!(
            parse_descriptor(&over).is_err(),
            "122 UTF-16-unit title (61 chars) accepted"
        );
        // label ≤80 UTF-16 units: 40 rockets == 80 (accept), 41 == 82 (refuse).
        let ok_field = format!("{{\"id\":\"f\",\"label\":\"{}\"}}", rocket.repeat(40));
        let ok = format!(
            "{{\"contract\":\"app-views/v1\",\"app\":\"d\",\"title\":\"t\",\"views\":[{{\"id\":\"v\",\"title\":\"t\",\"kind\":\"detail\",\"fields\":[{}]}}]}}",
            ok_field
        );
        assert!(
            parse_descriptor(&ok).is_ok(),
            "80 UTF-16-unit label refused"
        );
        let over_field = format!("{{\"id\":\"f\",\"label\":\"{}\"}}", rocket.repeat(41));
        let over = format!(
            "{{\"contract\":\"app-views/v1\",\"app\":\"d\",\"title\":\"t\",\"views\":[{{\"id\":\"v\",\"title\":\"t\",\"kind\":\"detail\",\"fields\":[{}]}}]}}",
            over_field
        );
        assert!(
            parse_descriptor(&over).is_err(),
            "82 UTF-16-unit label (41 chars) accepted"
        );
    }

    /// `summary` parity with contract.ts `text()`: optional, but a
    /// present non-string (incl. `null`) refuses — absent is the only
    /// "none". Pins the exact optionality the consumer enforces.
    #[test]
    fn summary_is_optional_never_null() {
        let view = "{\"id\":\"v\",\"title\":\"t\",\"kind\":\"detail\",\"fields\":[{\"id\":\"f\",\"label\":\"l\"}]}";
        // Absent summary parses and stays absent.
        let none = format!(
            "{{\"contract\":\"app-views/v1\",\"app\":\"d\",\"title\":\"t\",\"views\":[{}]}}",
            view
        );
        assert_eq!(parse_descriptor(&none).unwrap().summary, None);
        // A real string parses.
        let some = format!(
            "{{\"contract\":\"app-views/v1\",\"app\":\"d\",\"title\":\"t\",\"summary\":\"hello\",\"views\":[{}]}}",
            view
        );
        assert_eq!(
            parse_descriptor(&some).unwrap().summary.as_deref(),
            Some("hello")
        );
        // null / empty / non-string all refuse.
        for bad in ["null", "\"\"", "0", "[]", "{}"] {
            let input = format!(
                "{{\"contract\":\"app-views/v1\",\"app\":\"d\",\"title\":\"t\",\"summary\":{},\"views\":[{}]}}",
                bad, view
            );
            assert!(parse_descriptor(&input).is_err(), "summary {bad} accepted");
        }
    }

    #[test]
    fn oversized_serialized_input_refuses_before_parse() {
        let big = format!(
            r#"{{"contract":"app-views/v1","app":"d","title":"{}","views":[{{"id":"v","title":"t","kind":"detail","fields":[{{"id":"f","label":"l"}}]}}]}}"#,
            "x".repeat(65 * 1024)
        );
        assert!(parse_descriptor(&big).is_err());
    }
}
