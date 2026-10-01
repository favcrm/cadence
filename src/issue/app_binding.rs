//! `app-bindings/v1` companion contract validation (CAD-867, toward
//! CAD-811).
//!
//! A package whose `views/app-views-v1.json` descriptor (CAD-864) asks
//! the host to serve *live* verified data ships one companion file at
//! `bindings/app-bindings-v1.json`, declared by
//! `needs.bindings.contract` in `app.md`. This module is the Rust gate
//! the install/upgrade paths run before those bytes are trusted; the
//! grammar is documented in
//! `contracts/app-bindings/v1/README.md` and mirrored by
//! `contracts/app-bindings/v1/app-bindings.schema.json`.
//!
//! A binding file is data, never behaviour: it maps view ids the
//! descriptor already declares onto a closed set of host read
//! `sources` (`customers`, `caption-runs`) and a closed set of read
//! `ops` (`list`, `show`), and per field id a dotted `key` into that
//! source's fixed projection plus the descriptor `format` the host
//! must render it with. It can never name an installation, actor,
//! scope, URL, connection, query or write — the same authority keys
//! `app_view` forbids are forbidden recursively here, plus the keys
//! (`method`, `call`, `rpc`, `tool`, `command`, `exec`, `args`,
//! `binding`, `write`, `delete`, `send`, `request`, `fetch`, `body`,
//! `account`, `slot`, `params`, `where`, `order_by`, `limit`,
//! `cursor`) that would turn a data mapping into an invocation. The
//! host always selects the installation and context; `list`/`show` are
//! the only operations v1 knows, and `form` views — disabled previews
//! — can never acquire a binding. Optional producer fields that are
//! absent on a record (`email`/`phone`/`source`/`consent.sms`) or null
//! on a contextless run (`context_id`, `snapshot.context.id`), or
//! absent on a workflow that never declares the input
//! (`snapshot.inputs.subject`), are **omitted** cells — the live
//! adapter emits no key rather than a raw `null`, and the descriptor's
//! renderer shows the empty mark.
//!
//! Validation is two-layer: [`parse_binding`] checks the file's own
//! grammar and bounds; [`validate_against`] additionally requires the
//! parsed manifest and descriptor of the SAME bundle snapshot, so a
//! binding can never pair itself to a descriptor that was not
//! installed with it.

use serde_json::Value;
use std::collections::BTreeSet;

use crate::error::{Error, Result};
use crate::issue::{app::Manifest, app_view};

/// The contract tag — the filename and this string pin the version.
pub const CONTRACT: &str = "app-bindings/v1";

/// The one file a bundle may carry under `bindings/`.
pub const FILE: &str = "app-bindings-v1.json";

/// The bundle-relative path `bundle_files`/`snapshot` produce.
pub const REL_PATH: &str = "bindings/app-bindings-v1.json";

/* Bounds mirroring contract README/schema — keep them identical. */
const MAX_BINDINGS: usize = 16;
const MAX_FIELDS_PER_BINDING: usize = 24;
const MAX_OPS_PER_BINDING: usize = 2;
const MAX_ID_LENGTH: usize = 64;
const MAX_TITLE_LENGTH: usize = 120;
const MAX_SERIALIZED_BYTES: usize = 16 * 1024;
const MAX_NODES: usize = 1024;
const MAX_DEPTH: usize = 24;

/// The closed set of host read sources a v1 binding may name. Each
/// source owns a fixed projection the `field.key` paths address —
/// `customers` projects the stored `CustomerProfile` (plus the host's
/// `record_id` handle), `caption-runs` projects the run row and the
/// metadata-only `app_run_show` surface (never artifact content).
const SOURCES: &[&str] = &["customers", "caption-runs"];

/// The closed set of read operations. v1 has no write, dispatch,
/// release, import or arbitrary-method vocabulary at all.
const OPS: &[&str] = &["list", "show"];

/// The display formats a mapped field may declare — exactly the
/// `app-views/v1` field-format vocabulary, no wider grammar. A host
/// digest string (`snapshot_digest`) projects as plain `text` — v1
/// has no digest renderer.
const FORMATS: &[&str] = &["text", "number", "date", "datetime", "enum", "tags"];

/// `list`/`show` apply to different descriptor view kinds: a `table`
/// reads through `list`, a `detail` through `show`. Both accept either
/// operation name, but a bound op must be usable by the view kind the
/// descriptor declared — checked in `validate_against`.
const VIEW_KINDS: &[&str] = &["table", "detail"];

const BINDING_KEYS: &[&str] = &["view", "source", "ops", "fields"];
const FIELD_KEYS: &[&str] = &["field", "key", "format"];
const BINDING_FILE_KEYS: &[&str] = &["contract", "app", "title", "bindings"];

/// How one projected value actually arrives — the producer's real
/// shape, never the package's claim. `Text` is a scalar string
/// (optional on the producer — an absent/NULL cell is omitted by the
/// adapter and renders as the host's empty mark); `Tags` is a list of
/// tag strings (the only list-shaped producer); `Consent` is the
/// `CustomerConsent` state string (`granted`/`denied`/`unknown`, SMS
/// may be absent → omitted); `Digest` is a sha256 hex string rendered
/// as text; `Enum` supplies a fixed domain listed in the entry. A
/// scalar producer can only fill a `scalar` kind and a scalar format
/// it honestly produces — never `number`/`date`/`datetime` it cannot
/// guarantee, and never a `list` kind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Produced {
    Text,
    Tags,
    Consent,
    Digest,
    Enum(&'static [&'static str]),
}

/// The exact projection a binding may name per source, with each
/// key's produced shape — every path a host read adapter is allowed
/// to resolve. A `key` outside this table, or a `field`/`format` the
/// produced shape cannot honestly fill, refuses; a package can never
/// widen a read past the reviewed projection or mislabel its type.
///
/// `customers` → the record store's `CustomerProfile` (schema 1):
/// `record_id` is the host's row handle; `consent.email`/`consent.sms`
/// serialize `granted`/`denied`/`unknown` (`Consent` — descriptor
/// format must be `enum` with exactly that domain).
///
/// `caption-runs` → the run row plus `app_run_show`'s metadata-only
/// surface: `snapshot.workflow.title`, `snapshot.inputs.subject`,
/// `snapshot.context.id`. `state` is the run-state enum
/// (`awaiting_approval`/`approved`/`running`/`succeeded`/`failed`/
/// `cancelled`). `context_id`/`snapshot.context.id` are null on a
/// contextless run and `snapshot.inputs.subject` is absent on
/// workflows that never declare it (e.g. `source-instagram`'s
/// `profile_handle`) — the adapter omits those cells. The artifact
/// surface is deliberately absent — artifact ids, digests, types,
/// sizes and content are never projected v1.
const SOURCE_KEYS: &[(&str, &[(&str, Produced)])] = &[
    (
        "customers",
        &[
            // `record_id` is the host adapter's rename of the record
            // row's `id` field — the API response calls it `id`; the
            // binding's key names the projected handle.
            ("record_id", Produced::Text),
            ("display_name", Produced::Text),
            ("email", Produced::Text),
            ("phone", Produced::Text),
            ("tags", Produced::Tags),
            ("source", Produced::Text),
            ("consent.email", Produced::Consent),
            ("consent.sms", Produced::Consent),
        ],
    ),
    (
        "caption-runs",
        &[
            ("id", Produced::Text),
            (
                "state",
                Produced::Enum(&[
                    "awaiting_approval",
                    "approved",
                    "running",
                    "succeeded",
                    "failed",
                    "cancelled",
                ]),
            ),
            ("context_id", Produced::Text),
            ("snapshot_digest", Produced::Digest),
            ("snapshot.workflow.title", Produced::Text),
            ("snapshot.inputs.subject", Produced::Text),
            ("snapshot.context.id", Produced::Text),
        ],
    ),
];

/// Keys a binding file may never carry, recursively at every level.
/// The `app_view` authority list already covers identity/scope/URL/
/// credential/SQL escapes; the extra entries here close the invocation
/// vocabulary — a mapping names a host source, never a method, tool,
/// connection slot or filter the host did not publish.
const FORBIDDEN_BINDING_KEYS: &[&str] = &[
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
    // Invocation vocabulary — never selectable by package bytes.
    "method",
    "call",
    "rpc",
    "tool",
    "command",
    "exec",
    "args",
    "arguments",
    "binding",
    "connection",
    "account",
    "slot",
    "request",
    "request_id",
    "fetch",
    "body",
    "params",
    "where",
    "order_by",
    "limit",
    "cursor",
    "write",
    "update",
    "delete",
    "send",
    "approve",
    "dispatch",
    "run_id",
    "artifact",
    "artifacts",
    "snapshot",
    "record",
    "store",
    "migration",
];

fn fail(path: &str, message: impl Into<String>) -> Error {
    Error::rejected(format!("{path}: {}", message.into()))
}

/// The same control-character rule `app_view` applies — descriptor and
/// binding strings stay single-line plain text.
fn has_control(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{2028}' | '\u{2029}'))
}

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
    // UTF-16 code units, matching the consumer side — see app_view.
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
/// shape check, exactly like `app_view::scan_unsafe`, with the binding
/// contract's wider forbidden-key list and tighter node budget.
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
                if FORBIDDEN_BINDING_KEYS.contains(&key.as_str()) {
                    return Err(fail(path, format!("forbidden binding key: {key}")));
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

/// One validated field mapping: descriptor field id → dotted source
/// key + the format the host renders it with.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldBinding {
    pub field: String,
    pub key: String,
    pub format: String,
}

/// One validated view binding.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewBinding {
    pub view: String,
    pub source: String,
    pub ops: Vec<String>,
    pub fields: Vec<FieldBinding>,
}

/// The validated binding file. `raw` retains the parsed JSON for the
/// receipt — the binding is data, so the receipt serves the exact
/// reviewed bytes (as a parsed value), never a re-render.
#[derive(Clone, Debug)]
pub struct Binding {
    pub app: String,
    pub title: String,
    pub bindings: Vec<ViewBinding>,
    /// The validated JSON value — what the installation receipt serves.
    pub raw: Value,
}

/// What `key` produces on `source` — `None` when the pair is outside
/// the reviewed projection table.
fn produced(source: &str, key: &str) -> Option<Produced> {
    SOURCE_KEYS
        .iter()
        .find(|(name, _)| *name == source)?
        .1
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, p)| *p)
}

/// The descriptor field `key` a dotted source path names, split on
/// `.` — each segment must itself satisfy the identifier grammar so a
/// path can never smuggle a traversal or empty component.
fn key_path_valid(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_ID_LENGTH
        && key.split('.').all(|seg| {
            !seg.is_empty()
                && seg.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_')
                })
        })
}

fn parse_field_binding(v: &Value, path: &str, source: &str) -> Result<FieldBinding> {
    let map = expect_object(v, path)?;
    only_keys(map, FIELD_KEYS, path)?;
    let field = expect_ident(&v["field"], &format!("{path}.field"))?.to_string();
    let key = expect_string(&v["key"], &format!("{path}.key"), MAX_ID_LENGTH)?.to_string();
    if !key_path_valid(&key) {
        return Err(fail(
            &format!("{path}.key"),
            format!("unsafe source key path: {key:?}"),
        ));
    }
    if produced(source, &key).is_none() {
        return Err(fail(
            &format!("{path}.key"),
            format!("source '{source}' has no projection key '{key}'"),
        ));
    }
    let format = expect_string(&v["format"], &format!("{path}.format"), MAX_ID_LENGTH)?;
    if !FORMATS.contains(&format) {
        return Err(fail(
            &format!("{path}.format"),
            format!("expected one of {}", FORMATS.join(", ")),
        ));
    }
    Ok(FieldBinding {
        field,
        key,
        format: format.to_string(),
    })
}

fn parse_view_binding(v: &Value, path: &str) -> Result<ViewBinding> {
    let map = expect_object(v, path)?;
    only_keys(map, BINDING_KEYS, path)?;
    let view = expect_ident(&v["view"], &format!("{path}.view"))?.to_string();
    let source = expect_string(&v["source"], &format!("{path}.source"), MAX_ID_LENGTH)?;
    if !SOURCES.contains(&source) {
        return Err(fail(
            &format!("{path}.source"),
            format!("expected one of {}", SOURCES.join(", ")),
        ));
    }
    let raw_ops = map
        .get("ops")
        .ok_or_else(|| fail(&format!("{path}.ops"), "a binding needs its read ops"))?;
    let items = expect_array(raw_ops, &format!("{path}.ops"))?;
    if items.is_empty() {
        return Err(fail(
            &format!("{path}.ops"),
            "a binding needs at least one read op",
        ));
    }
    if items.len() > MAX_OPS_PER_BINDING {
        return Err(fail(
            &format!("{path}.ops"),
            format!("more than {MAX_OPS_PER_BINDING} ops"),
        ));
    }
    let mut ops = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let op = expect_string(item, &format!("{path}.ops.{i}"), MAX_ID_LENGTH)?;
        if !OPS.contains(&op) {
            return Err(fail(
                &format!("{path}.ops.{i}"),
                format!("expected one of {}", OPS.join(", ")),
            ));
        }
        if ops.iter().any(|o| o == op) {
            return Err(fail(
                &format!("{path}.ops.{i}"),
                format!("duplicate op: {op}"),
            ));
        }
        ops.push(op.to_string());
    }
    let raw_fields = map.get("fields").ok_or_else(|| {
        fail(
            &format!("{path}.fields"),
            "a binding needs at least one mapped field",
        )
    })?;
    let items = expect_array(raw_fields, &format!("{path}.fields"))?;
    if items.is_empty() {
        return Err(fail(
            &format!("{path}.fields"),
            "a binding needs at least one mapped field",
        ));
    }
    if items.len() > MAX_FIELDS_PER_BINDING {
        return Err(fail(
            &format!("{path}.fields"),
            format!("more than {MAX_FIELDS_PER_BINDING} fields"),
        ));
    }
    let mut fields = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
    for (i, item) in items.iter().enumerate() {
        let binding = parse_field_binding(item, &format!("{path}.fields.{i}"), source)?;
        if !seen.insert(binding.field.clone()) {
            return Err(fail(
                &format!("{path}.fields"),
                format!("duplicate field binding: {}", binding.field),
            ));
        }
        fields.push(binding);
    }
    Ok(ViewBinding {
        view,
        source: source.to_string(),
        ops,
        fields,
    })
}

/// Validate `text` as an `app-bindings/v1` file on its own grammar:
/// contract tag, closed key sets, bounded strings/arrays, forbidden
/// keys recursively, closed sources/ops, projection-key membership —
/// every refusal is bounded and names its dotted path. Cross-bundle
/// checks (manifest `app`, descriptor view/field/kind) live in
/// [`validate_against`]; this function is the byte-level gate.
pub fn parse_binding(text: &str) -> Result<Binding> {
    // File bytes, not re-serialized tree — same envelope rule the
    // descriptor gate applies (see app_view::parse_descriptor).
    if text.len() > MAX_SERIALIZED_BYTES {
        return Err(fail(
            "$",
            format!("input exceeds {MAX_SERIALIZED_BYTES} bytes"),
        ));
    }
    let raw: Value =
        serde_json::from_str(text).map_err(|e| fail("$", format!("binding is not JSON: {e}")))?;
    let mut nodes = 0usize;
    scan_unsafe(&raw, "$", &mut nodes, 0)?;
    let map = expect_object(&raw, "$")?;
    only_keys(map, BINDING_FILE_KEYS, "$")?;
    if raw["contract"] != Value::String(CONTRACT.to_string()) {
        return Err(fail("$.contract", format!("expected {CONTRACT:?}")));
    }
    let app = expect_ident(&raw["app"], "$.app")?.to_string();
    let title = expect_string(&raw["title"], "$.title", MAX_TITLE_LENGTH)?.to_string();
    let raw_bindings = map
        .get("bindings")
        .ok_or_else(|| fail("$.bindings", "a binding file needs at least one binding"))?;
    let items = expect_array(raw_bindings, "$.bindings")?;
    if items.is_empty() {
        return Err(fail(
            "$.bindings",
            "a binding file needs at least one binding",
        ));
    }
    if items.len() > MAX_BINDINGS {
        return Err(fail(
            "$.bindings",
            format!("more than {MAX_BINDINGS} bindings"),
        ));
    }
    let mut bindings = Vec::with_capacity(items.len());
    let mut views = BTreeSet::new();
    for (i, item) in items.iter().enumerate() {
        let binding = parse_view_binding(item, &format!("$.bindings.{i}"))?;
        if !views.insert(binding.view.clone()) {
            return Err(fail(
                "$.bindings",
                format!("duplicate view binding: {}", binding.view),
            ));
        }
        bindings.push(binding);
    }
    Ok(Binding {
        app,
        title,
        bindings,
        raw,
    })
}

/// The second layer: the binding can only name what the SAME bundle's
/// manifest and descriptor declare. `manifest` supplies the one true
/// `app` (binding `app` must equal it) and `descriptor` the closed set
/// of view ids, field ids and per-field formats. `descriptor` is
/// `Some` only when the bundle carries a parsed `app-views/v1`
/// descriptor — a binding requires it absolutely.
pub fn validate_against(
    binding: &Binding,
    manifest: &Manifest,
    descriptor: Option<&app_view::Descriptor>,
) -> Result<()> {
    if binding.app != manifest.app {
        return Err(fail(
            "$.app",
            format!(
                "binding `app` is '{}' — it must be this bundle's '{}'",
                binding.app, manifest.app
            ),
        ));
    }
    let descriptor = descriptor.ok_or_else(|| {
        fail(
            "$",
            "a bindings companion requires the bundle's app-views/v1 descriptor",
        )
    })?;
    // Same-app identity binds all three documents of the one reviewed
    // snapshot — the descriptor was parsed from this bundle, but the
    // check is cheap and keeps the public helper's promise complete.
    if descriptor.app != manifest.app {
        return Err(fail(
            "$.app",
            format!(
                "descriptor `app` is '{}' — it must be this bundle's '{}'",
                descriptor.app, manifest.app
            ),
        ));
    }
    for (i, binding) in binding.bindings.iter().enumerate() {
        let path = format!("$.bindings.{i}");
        let view = descriptor
            .views
            .iter()
            .find(|v| v.id == binding.view)
            .ok_or_else(|| {
                fail(
                    &path,
                    format!("binding names undeclared view id: {}", binding.view),
                )
            })?;
        // Forms are disabled previews — v1 wires no live submit, so a
        // binding can never attach to one.
        if !VIEW_KINDS.contains(&view.kind.as_str()) {
            return Err(fail(
                &path,
                format!("a \"{}\" view cannot carry a binding", view.kind),
            ));
        }
        // A bound op must be usable by the descriptor view's kind:
        // tables read through `list`, details through `show`.
        for op in &binding.ops {
            let usable = matches!(
                (view.kind.as_str(), op.as_str()),
                ("table", "list") | ("detail", "show")
            );
            if !usable {
                return Err(fail(
                    &path,
                    format!(
                        "op '{op}' is not a {} view's read — tables list, details show",
                        view.kind
                    ),
                ));
            }
        }
        let declared: std::collections::BTreeMap<&str, &app_view::Field> =
            view.fields.iter().map(|f| (f.id.as_str(), f)).collect();
        for (fi, field) in binding.fields.iter().enumerate() {
            let fpath = format!("{path}.fields.{fi}");
            let descriptor_field = declared.get(field.field.as_str()).ok_or_else(|| {
                fail(
                    &fpath,
                    format!(
                        "binding names undeclared field '{}' on view '{}'",
                        field.field, binding.view
                    ),
                )
            })?;
            // The binding's declared format must equal the
            // descriptor's — a field that says `enum` to the host and
            // `text` to the renderer is a confused-deputy in two
            // grammars.
            if field.format != descriptor_field.format {
                return Err(fail(
                    &fpath,
                    format!(
                        "field '{}' declares format '{}' but the descriptor declares '{}'",
                        field.field, field.format, descriptor_field.format
                    ),
                ));
            }
            // The declared format must also be one the produced shape
            // can honestly fill — the package never invents a producer
            // type, and a scalar producer can never claim `number`,
            // `date` or `datetime` it does not produce, or a `list`
            // kind. `tags` needs the source's list-of-tags; `enum`
            // needs the source's own declared domain.
            let shape_ok = match (produced(&binding.source, &field.key), field.format.as_str()) {
                // Scalar string producers fill only `text` — never a
                // number, calendar or digest renderer they cannot
                // honestly produce.
                (Some(Produced::Text) | Some(Produced::Digest), "text") => true,
                (Some(Produced::Tags), "tags") => true,
                (Some(Produced::Consent), "enum") => true,
                (Some(Produced::Enum(_)), "enum") => true,
                _ => false,
            };
            if !shape_ok {
                return Err(fail(
                    &fpath,
                    format!(
                        "field '{}' maps '{}:{}' — its produced shape cannot fill descriptor format '{}'",
                        field.field, binding.source, field.key, field.format
                    ),
                ));
            }
            // The produced shape must also match the descriptor's
            // scalar/list kind: `tags` is the only list-shaped
            // producer, everything else is scalar. A scalar mapped to
            // a descriptor `kind:"list"` would hand the cell
            // consumer a string where it demands an array.
            let kind_ok = match produced(&binding.source, &field.key) {
                Some(Produced::Tags) => descriptor_field.kind == "list",
                Some(_) => descriptor_field.kind == "scalar",
                None => false,
            };
            if !kind_ok {
                return Err(fail(
                    &fpath,
                    format!(
                        "field '{}' maps '{}:{}' — its produced shape is not descriptor kind '{}'",
                        field.field, binding.source, field.key, descriptor_field.kind
                    ),
                ));
            }
            // Enum domains are closed at both ends: the descriptor's
            // declared `values` must equal the produced domain exactly,
            // so a package can neither widen nor rename the host's
            // vocabulary.
            let produced = produced(&binding.source, &field.key);
            let domain: Option<&[&str]> = match produced {
                Some(Produced::Enum(domain)) => Some(domain),
                Some(Produced::Consent) => Some(&["granted", "denied", "unknown"]),
                _ => None,
            };
            if let Some(domain) = domain {
                let declared: Vec<&str> = descriptor_field
                    .values
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(String::as_str)
                    .collect();
                if declared != domain {
                    return Err(fail(
                        &fpath,
                        format!(
                            "field '{}' maps '{}:{}' — descriptor enum values must be exactly {}",
                            field.field,
                            binding.source,
                            field.key,
                            domain.join(", ")
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// One-shot validation of a binding file against its own bundle's
/// manifest and descriptor — what install, upgrade and installed
/// readback all run. The caller supplies the descriptor text of the
/// SAME snapshot (already required to exist when a binding does).
pub fn parse_and_validate(
    binding_text: &str,
    manifest: &Manifest,
    descriptor_text: &str,
) -> Result<Binding> {
    let binding = parse_binding(binding_text)?;
    let descriptor = app_view::parse_descriptor(descriptor_text)
        .map_err(|e| fail("$", format!("companion descriptor is invalid: {e}")))?;
    validate_against(&binding, manifest, Some(&descriptor))?;
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTOR: &str = r#"{"contract":"app-views/v1","app":"crm","title":"CRM","views":[
        {"id":"customers","title":"Customers","kind":"table","fields":[
            {"id":"name","label":"Name","format":"text"},
            {"id":"email","label":"Email","format":"text"},
            {"id":"tags","label":"Tags","format":"tags","kind":"list"},
            {"id":"consent_email","label":"Email consent","format":"enum","values":["granted","denied","unknown"]}
        ],"columns":[{"field":"name"},{"field":"email"}]},
        {"id":"customer-detail","title":"Customer","kind":"detail","fields":[
            {"id":"name","label":"Name","format":"text"},
            {"id":"phone","label":"Phone","format":"text"}
        ]},
        {"id":"customer-form","title":"New customer","kind":"form","previewOf":[
            {"id":"name","label":"Full name","format":"text"}
        ]}
    ]}"#;

    const BINDING: &str = r#"{"contract":"app-bindings/v1","app":"crm","title":"CRM bindings","bindings":[
        {"view":"customers","source":"customers","ops":["list"],"fields":[
            {"field":"name","key":"display_name","format":"text"},
            {"field":"email","key":"email","format":"text"},
            {"field":"tags","key":"tags","format":"tags"},
            {"field":"consent_email","key":"consent.email","format":"enum"}
        ]},
        {"view":"customer-detail","source":"customers","ops":["show"],"fields":[
            {"field":"name","key":"display_name","format":"text"},
            {"field":"phone","key":"phone","format":"text"}
        ]}
    ]}"#;

    const BINDING_RUNS: &str = r#"{"contract":"app-bindings/v1","app":"social-content","title":"Run bindings","bindings":[
        {"view":"runs","source":"caption-runs","ops":["list"],"fields":[
            {"field":"id","key":"id","format":"text"},
            {"field":"state","key":"state","format":"enum"},
            {"field":"subject","key":"snapshot.inputs.subject","format":"text"}
        ]}
    ]}"#;
    const DESCRIPTOR_RUNS: &str = r#"{"contract":"app-views/v1","app":"social-content","title":"Runs","views":[
        {"id":"runs","title":"Caption runs","kind":"table","fields":[
            {"id":"id","label":"Run","format":"text"},
            {"id":"state","label":"State","format":"enum","values":["awaiting_approval","approved","running","succeeded","failed","cancelled"]},
            {"id":"subject","label":"Subject","format":"text"}
        ],"columns":[{"field":"id"},{"field":"state"}]}
    ]}"#;

    fn manifest() -> Manifest {
        Manifest {
            app: "crm".to_string(),
            title: "CRM".to_string(),
            version: "0.1.0".to_string(),
            connections: vec![],
            capabilities: Default::default(),
            summary: None,
            view_contract: Some(app_view::CONTRACT.to_string()),
            binding_contract: Some(CONTRACT.to_string()),
            guide: String::new(),
        }
    }

    fn descriptor() -> app_view::Descriptor {
        app_view::parse_descriptor(DESCRIPTOR).unwrap()
    }

    fn run_manifest() -> Manifest {
        Manifest {
            app: "social-content".to_string(),
            title: "Social".to_string(),
            version: "0.5.0".to_string(),
            connections: vec![],
            capabilities: Default::default(),
            summary: None,
            view_contract: Some(app_view::CONTRACT.to_string()),
            binding_contract: Some(CONTRACT.to_string()),
            guide: String::new(),
        }
    }

    #[test]
    fn parses_and_cross_validates_a_real_crm_binding() {
        let binding = parse_and_validate(BINDING, &manifest(), DESCRIPTOR).unwrap();
        assert_eq!(binding.app, "crm");
        assert_eq!(binding.bindings.len(), 2);
        assert_eq!(binding.bindings[0].source, "customers");
        assert_eq!(binding.bindings[0].fields[3].key, "consent.email");
        assert_eq!(binding.bindings[1].ops, vec!["show".to_string()]);
        let runs = parse_and_validate(BINDING_RUNS, &run_manifest(), DESCRIPTOR_RUNS).unwrap();
        assert_eq!(runs.bindings[0].source, "caption-runs");
        assert_eq!(runs.bindings[0].fields[2].key, "snapshot.inputs.subject");
    }

    /// Producer-shape honesty: a format that the real produced value
    /// cannot fill refuses even when it satisfies the descriptor's
    /// declared format. These cases pin the type matrix.
    #[test]
    fn produced_shape_refuses_mismatched_formats() {
        let manifest = manifest();
        let descriptor = descriptor();
        let base: Value = serde_json::from_str(BINDING).unwrap();
        // A scalar source key cannot fill the descriptor's list-typed
        // `tags` field — format matches the descriptor, produced shape
        // does not.
        let mut bad = base.clone();
        bad["bindings"][0]["fields"][2] =
            serde_json::json!({"field":"tags","key":"display_name","format":"tags"});
        let binding = parse_binding(&bad.to_string()).unwrap();
        let err = validate_against(&binding, &manifest, Some(&descriptor)).unwrap_err();
        assert!(err.to_string().contains("produced shape"), "{err}");
        // A consent-shaped key cannot fill a text field — only `enum`
        // with the consent domain may carry it.
        let mut bad = base.clone();
        bad["bindings"][0]["fields"][3] =
            serde_json::json!({"field":"consent_email","key":"phone","format":"enum"});
        let binding = parse_binding(&bad.to_string()).unwrap();
        let err = validate_against(&binding, &manifest, Some(&descriptor)).unwrap_err();
        assert!(err.to_string().contains("produced shape"), "{err}");
        // A scalar text key can never claim a `number`/`date`/`datetime`
        // format — the produced string cannot honestly fill them.
        for (key, fmt) in [
            ("display_name", "number"),
            ("display_name", "date"),
            ("display_name", "datetime"),
        ] {
            let mut bad = base.clone();
            bad["bindings"][0]["fields"][0] =
                serde_json::json!({"field":"name","key":key,"format":fmt});
            // Descriptor field `name` stays text — a mismatched declared
            // format already refuses; test against a descriptor whose
            // `name` field actually declares that format so only the
            // produced-shape rule fires.
            let desc = app_view::parse_descriptor(&DESCRIPTOR.replace(
                "{\"id\":\"name\",\"label\":\"Name\",\"format\":\"text\"}",
                &format!("{{\"id\":\"name\",\"label\":\"Name\",\"format\":\"{fmt}\"}}"),
            ));
            // `number`/`date`/`datetime` fields are legal v1 grammar —
            // the refusal must come from produced shape, not parse.
            let desc = match desc {
                Ok(d) => d,
                Err(_) => continue,
            };
            let binding = parse_binding(&bad.to_string()).unwrap();
            let err = validate_against(&binding, &manifest, Some(&desc)).unwrap_err();
            assert!(
                err.to_string().contains("produced shape"),
                "{key}→{fmt} refused for the wrong reason: {err}"
            );
        }
        // An enum binding whose declared domain does not equal the
        // produced domain refuses (widened or narrowed).
        let mut bad = serde_json::from_str::<Value>(BINDING_RUNS).unwrap();
        bad["bindings"][0]["fields"][1] =
            serde_json::json!({"field":"state","key":"state","format":"enum"});
        let widened = app_view::parse_descriptor(&DESCRIPTOR_RUNS.replace(
            "\"values\":[\"awaiting_approval\",\"approved\",\"running\",\"succeeded\",\"failed\",\"cancelled\"]",
            "\"values\":[\"awaiting_approval\",\"approved\",\"running\",\"succeeded\",\"failed\",\"cancelled\",\"bogus\"]",
        ));
        // widened descriptor parses but mismatches the produced domain
        let run_desc = widened.unwrap();
        let binding = parse_binding(&bad.to_string()).unwrap();
        assert!(
            validate_against(&binding, &run_manifest(), Some(&run_desc)).is_err(),
            "widened enum domain must refuse"
        );
    }

    /// The produced scalar/list shape must match the descriptor field's
    /// declared `kind` — a scalar `display_name` can never fill a
    /// `kind:"list"` cell (the consumer calls `stringList` on it).
    #[test]
    fn produced_shape_refuses_a_scalar_on_a_list_field() {
        let manifest = manifest();
        // Descriptor that declares `name` as a text list (v1 allows
        // text kind:"list"); bind it to a scalar source key.
        let desc = app_view::parse_descriptor(&DESCRIPTOR.replace(
            "{\"id\":\"name\",\"label\":\"Name\",\"format\":\"text\"}",
            "{\"id\":\"name\",\"label\":\"Name\",\"format\":\"text\",\"kind\":\"list\"}",
        ))
        .unwrap();
        let mut bad = serde_json::from_str::<Value>(BINDING).unwrap();
        bad["bindings"][0]["fields"][0] =
            serde_json::json!({"field":"name","key":"display_name","format":"text"});
        let binding = parse_binding(&bad.to_string()).unwrap();
        let err = validate_against(&binding, &manifest, Some(&desc)).unwrap_err();
        assert!(err.to_string().contains("kind"), "{err}");
        // The same kind mismatch in reverse — `tags` (a real list) on a
        // scalar field — is already covered by the format check, but the
        // kind check stands alone: a `tags` key on a descriptor field
        // wrongly declared scalar is refused by the format rule too.
    }

    #[test]
    fn refuses_each_grammar_violation() {
        let cases: Vec<(&str, &str)> = vec![
            ("{}", "contract"),
            // wrong contract tag
            (
                r#"{"contract":"app-bindings/v2","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}]}]}"#,
                "contract",
            ),
            // unknown top-level key
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","extra":1,"bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}]}]}"#,
                "unknown key",
            ),
            // closed source / op allowlists
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"orders","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}]}]}"#,
                "expected one of",
            ),
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["delete"],"fields":[{"field":"name","key":"display_name","format":"text"}]}]}"#,
                "expected one of",
            ),
            // projection key not in the source's closed table
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"password","format":"text"}]}]}"#,
                "no projection key",
            ),
            // a traversal-looking / empty-segment key
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name..x","format":"text"}]}]}"#,
                "unsafe source key",
            ),
            // undeclared field id and duplicate field binding
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"},{"field":"name","key":"email","format":"text"}]}]}"#,
                "duplicate field binding",
            ),
            // unknown binding-level key
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}],"actor":"x"}]}]"#,
                "unknown key",
            ),
            // forbidden keys — authority and invocation vocabulary
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}],"install_id":"x"}]}"#,
                "forbidden binding key",
            ),
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","install_id":"x","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}]}]}"#,
                "forbidden binding key",
            ),
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text","token":"t"}]}]}"#,
                "forbidden binding key",
            ),
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}],"method":"app_record_delete"}]}"#,
                "forbidden binding key",
            ),
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}],"url":"https://x"}]}"#,
                "forbidden binding key",
            ),
            // duplicate view binding
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}]},{"view":"customers","source":"customers","ops":["list"],"fields":[{"field":"name","key":"display_name","format":"text"}]}]}"#,
                "duplicate view binding",
            ),
            // empty bindings
            (
                r#"{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[]}"#,
                "at least one binding",
            ),
            // not JSON / oversized
            ("not json", "not JSON"),
        ];
        for (input, why) in cases {
            assert!(parse_binding(input).is_err(), "accepted {why}: {input}");
        }
    }

    /// Every forbidden key refuses both at the binding-file root and
    /// nested inside a field mapping — the recursive scan is the same
    /// depth-first gate app_view applies.
    #[test]
    fn forbidden_keys_all_refuse_at_root_and_nested() {
        for key in FORBIDDEN_BINDING_KEYS {
            let bad = format!(
                r#"{{"contract":"app-bindings/v1","app":"crm","title":"t","{key}":"x","bindings":[{{"view":"customers","source":"customers","ops":["list"],"fields":[{{"field":"name","key":"display_name","format":"text"}}]}}]}}"#
            );
            assert!(
                parse_binding(&bad).is_err(),
                "forbidden key {key} accepted at root"
            );
            let nested = format!(
                r#"{{"contract":"app-bindings/v1","app":"crm","title":"t","bindings":[{{"view":"customers","source":"customers","ops":["list"],"fields":[{{"field":"name","key":"display_name","format":"text","{key}":"x"}}]}}]}}"#
            );
            assert!(
                parse_binding(&nested).is_err(),
                "forbidden key {key} accepted nested"
            );
        }
    }

    /// Cross-validation: the binding can only reach what the same
    /// bundle's descriptor declared — unknown view, unknown field, a
    /// form view (no live submit v1), the wrong op for the view kind,
    /// a format lying about the descriptor's type, or a different app.
    /// Each case edits the parsed JSON so no string replace can drift.
    #[test]
    fn cross_validation_refuses_orphans_and_lies() {
        let manifest = manifest();
        let descriptor = descriptor();
        let base: Value = serde_json::from_str(BINDING).unwrap();
        let cases: Vec<(Value, &str)> = vec![
            // undeclared view id
            (
                {
                    let mut v = base.clone();
                    v["bindings"][0]["view"] = Value::String("orders".into());
                    v
                },
                "undeclared view id",
            ),
            // a form view can never be bound
            (
                {
                    let mut v = base.clone();
                    v["bindings"][0]["view"] = Value::String("customer-form".into());
                    v
                },
                "cannot carry a binding",
            ),
            // undeclared field on the view
            (
                {
                    let mut v = base.clone();
                    v["bindings"][1]["fields"][1]["field"] = Value::String("ghost".into());
                    v
                },
                "undeclared field",
            ),
            // op not usable by the view kind (detail cannot list)
            (
                {
                    let mut v = base.clone();
                    v["bindings"][1]["ops"] = serde_json::json!(["list"]);
                    v
                },
                "not a detail view's read",
            ),
            // declared format must match the descriptor's field format
            (
                {
                    let mut v = base.clone();
                    v["bindings"][0]["fields"][3]["format"] = Value::String("text".into());
                    v
                },
                "declares format",
            ),
            // binding app must equal manifest app
            (
                {
                    let mut v = base.clone();
                    v["app"] = Value::String("other".into());
                    v
                },
                "must be this bundle's",
            ),
        ];
        for (value, why) in cases {
            let binding = parse_binding(&value.to_string())
                .unwrap_or_else(|_| panic!("{why}: bad test fixture"));
            assert!(
                validate_against(&binding, &manifest, Some(&descriptor)).is_err(),
                "cross-validation accepted {why}"
            );
        }
    }

    /// A binding without its descriptor is refused outright — the
    /// companion file can never stand alone.
    #[test]
    fn binding_requires_the_descriptor() {
        let binding = parse_binding(BINDING).unwrap();
        assert!(validate_against(&binding, &manifest(), None).is_err());
    }

    /// `parse_and_validate` is the complete same-snapshot gate: a
    /// descriptor naming a different app than the manifest refuses
    /// even when the binding itself is honest.
    #[test]
    fn helper_refuses_a_cross_app_descriptor() {
        let wrong_app = DESCRIPTOR.replace("\"app\":\"crm\"", "\"app\":\"other\"");
        assert!(app_view::parse_descriptor(&wrong_app).is_ok());
        assert!(parse_and_validate(BINDING, &manifest(), &wrong_app).is_err());
    }

    /// The serialized cap counts file bytes before parse — a >16 KiB
    /// input refuses regardless of tree shape.
    #[test]
    fn oversized_input_refuses_before_parse() {
        let big = format!(
            r#"{{"contract":"app-bindings/v1","app":"crm","title":"{}","bindings":[{{"view":"customers","source":"customers","ops":["list"],"fields":[{{"field":"name","key":"display_name","format":"text"}}]}}]}}"#,
            "x".repeat(17 * 1024)
        );
        assert!(parse_binding(&big).is_err());
    }

    /// The shipped contract examples must validate end-to-end against
    /// their matching `app-views/v1` examples — the two published
    /// documents agree on view ids, field ids, formats and apps, so a
    /// drift between the schema and the examples fails the suite.
    #[test]
    fn shipped_examples_validate_against_their_descriptors() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for app in ["crm", "social-content"] {
            let descriptor = std::fs::read_to_string(
                root.join(format!("contracts/app-views/v1/examples/{app}.json")),
            )
            .unwrap();
            let binding = std::fs::read_to_string(
                root.join(format!("contracts/app-bindings/v1/examples/{app}.json")),
            )
            .unwrap();
            let manifest = Manifest {
                app: app.to_string(),
                title: app.to_string(),
                version: "1".to_string(),
                connections: vec![],
                capabilities: Default::default(),
                summary: None,
                view_contract: Some(app_view::CONTRACT.to_string()),
                binding_contract: Some(CONTRACT.to_string()),
                guide: String::new(),
            };
            parse_and_validate(&binding, &manifest, &descriptor)
                .unwrap_or_else(|e| panic!("{app} example must validate: {e}"));
        }
    }
}
