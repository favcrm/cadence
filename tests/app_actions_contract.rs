//! CAD-964 (toward CAD-811): the app-actions/v1 metadata contract.
//!
//! This test is the contract's own integrity and adversarial gate. It
//! exercises `issue::app_action::parse` — a pure, data-only
//! parser/validator that decides whether untrusted JSON bytes are a
//! well-formed action descriptor. Nothing here executes an action,
//! opens an installation, admits a manifest key, or touches a route:
//! execution, admission (`needs.actions`), receipt emission and the
//! dispatcher are all later CAD-811 slices on the open CAD-864 seam.
//!
//! Three layers are checked, each on both sides of accept/refuse:
//!
//! - every published file under `contracts/app-actions/v1/` — the JSON
//!   Schema compiles, and every file in `examples/` validates against
//!   the schema AND parses through the Rust validator, so the two
//!   notations can never silently disagree;
//! - refusal vectors that mutate a valid descriptor — wrong contract
//!   tag, forged authority keys (`install_id`/`actor`/`digest`…),
//!   duplicate ids, unknown operations, oversize payloads — each must
//!   fail closed;
//! - size-bound cases at and just past each limit.
//!
//! The parser admits data only. Whether a host ever *honors* a
//! descriptor is decided by operator approval, admission and execution
//! gates this slice deliberately does not build.

use std::path::{Path, PathBuf};

use cadence_agent::issue::app_action;
use serde_json::{json, Value};

fn contract_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("contracts/app-actions/v1")
}

fn load(name: &str) -> Value {
    let path = contract_dir().join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn load_example(name: &str) -> Value {
    load(&format!("examples/{name}"))
}

fn parse(raw: &Value) -> Result<app_action::Descriptor, String> {
    app_action::parse(raw).map_err(|e| e.to_string())
}

fn parse_ok(raw: &Value) -> app_action::Descriptor {
    parse(raw).unwrap_or_else(|e| panic!("expected a valid descriptor: {e}"))
}

fn parse_err(raw: &Value) -> String {
    match parse(raw) {
        Ok(_) => panic!("expected refusal, got: {}", raw),
        Err(e) => e,
    }
}

/// Every published example must be accepted by both the JSON Schema
/// and the Rust parser — the two notations are one grammar, and a
/// drift between them means one side of the seam lies.
#[test]
fn published_examples_validate_and_parse() {
    let schema = load("app-actions.schema.json");
    let validator = jsonschema::validator_for(&schema)
        .unwrap_or_else(|e| panic!("schema does not compile: {e}"));
    for name in ["crm.json", "ledger.json"] {
        let raw = load_example(name);
        let errors: Vec<String> = validator.iter_errors(&raw).map(|e| e.to_string()).collect();
        assert!(
            errors.is_empty(),
            "examples/{name} fails the schema: {errors:?}"
        );
        // Schema-valid is not sufficient — the consumer's closed parse
        // must also accept (unique ids, cross-references, forbidden
        // keys live beyond JSON Schema's reach).
        let descriptor = parse_ok(&raw);
        assert_eq!(descriptor.app, raw["app"].as_str().unwrap());
        assert!(
            !descriptor.actions.is_empty(),
            "examples/{name} declares no actions"
        );
    }
}

/// The schema itself is a contract artifact: it must compile as a
/// draft-2020-12 schema or a malformed schema would silently gate on
/// nothing.
#[test]
fn schema_compiles() {
    let schema = load("app-actions.schema.json");
    jsonschema::validator_for(&schema).unwrap_or_else(|e| panic!("schema does not compile: {e}"));
}

/// A minimal, independently-constructed descriptor — not derived from
/// the published examples — proves the grammar is not keyed to one app
/// name or one record kind. `app` is provenance only.
#[test]
fn accepts_a_second_app_and_record_kind() {
    let raw = json!({
        "contract": "app-actions/v1",
        "app": "fleet",
        "title": "Fleet — vehicle records",
        "actions": [
            {
                "id": "vehicle.register",
                "title": "Register vehicle",
                "operation": "record.create",
                "record": "vehicle",
                "input": {
                    "fields": [
                        { "id": "plate", "label": "Plate" },
                        { "id": "class", "label": "Class", "type": "enum", "values": ["car", "van"] }
                    ]
                }
            }
        ]
    });
    let d = parse_ok(&raw);
    assert_eq!(d.app, "fleet");
    assert_eq!(d.actions[0].id, "vehicle.register");
    assert_eq!(d.actions[0].operation, app_action::Operation::RecordCreate);
    assert_eq!(d.actions[0].record, "vehicle");
    assert_eq!(d.actions[0].input.fields.len(), 2);
}

/// The contract tag is the version: anything else — including a later
/// draft's tag or a near-miss — refuses.
#[test]
fn refuses_wrong_or_missing_contract_tag() {
    for tag in [
        json!("app-actions/v2"),
        json!("app-views/v1"),
        json!("app-actions"),
        json!("v1"),
        json!(""),
        json!(1),
        Value::Null,
    ] {
        let mut raw = load_example("crm.json");
        raw["contract"] = tag.clone();
        parse_err(&raw);
    }
    let mut raw = load_example("crm.json");
    raw.as_object_mut().unwrap().remove("contract");
    parse_err(&raw);
}

/// `app` is provenance, never an installation/context/actor. A name
/// outside the identifier grammar — or one naming a host object —
/// refuses.
#[test]
fn refuses_unsafe_app_names() {
    for app in [
        json!("CRM"),     // case
        json!("crm app"), // space
        json!("crm/app"), // path separator
        json!("-crm"),    // leading dash
        json!("_crm"),
        json!("9crm"),
        json!("a".repeat(65)), // over 64
        json!(""),
    ] {
        let mut raw = load_example("crm.json");
        raw["app"] = app;
        parse_err(&raw);
    }
    let mut raw = load_example("crm.json");
    raw.as_object_mut().unwrap().remove("app");
    parse_err(&raw);
}

/// Unknown top-level keys refuse — the descriptor is closed, so a
/// package cannot smuggle an extra declaration past review.
#[test]
fn refuses_unknown_top_level_keys() {
    for key in [
        "views",
        "routes",
        "install",
        "install_id",
        "context",
        "context_id",
        "workspace",
        "project",
        "actor",
        "by",
        "role",
        "grant",
        "scope",
        "scopes",
        "capability",
        "capabilities",
        "effect",
        "effects",
        "verified",
        "digest",
        "revision",
        "secret",
        "token",
        "url",
        "endpoint",
        "sql",
        "path",
        "script",
    ] {
        let mut raw = load_example("crm.json");
        raw.as_object_mut()
            .unwrap()
            .insert(key.to_string(), json!("x"));
        let e = parse_err(&raw);
        assert!(
            e.contains(key) || e.contains("forbidden") || e.contains("unknown"),
            "{key}: {e}"
        );
    }
}

/// Authority keys are forbidden *recursively* — a forged `install_id`,
/// `actor`, `digest` or `revision` nested anywhere in the tree is a
/// refusal, not just at the top level.
#[test]
fn refuses_forged_authority_at_any_depth() {
    let bases: Vec<Value> = vec![load_example("crm.json"), load_example("ledger.json")];
    for base in bases {
        for &key in app_action::FORBIDDEN_DESCRIPTOR_KEYS {
            // On an action object.
            let mut raw = base.clone();
            raw["actions"][0]
                .as_object_mut()
                .unwrap()
                .insert(key.to_string(), json!("x"));
            parse_err(&raw);
            // On an input field.
            let mut raw = base.clone();
            raw["actions"][0]["input"]["fields"][0]
                .as_object_mut()
                .unwrap()
                .insert(key.to_string(), json!("x"));
            parse_err(&raw);
            // Deep inside a value: a forbidden key nested under a
            // non-declared object on a field. The recursive scan runs
            // before the shape checks, so the forbidden key names
            // itself rather than the enclosing unknown key.
            let mut raw = base.clone();
            raw["actions"][0]["input"]["fields"][0] = json!({
                "id": "x", "label": "X", "type": "text",
                "meta": { "nested": { key: "forged" } }
            });
            let e = parse_err(&raw);
            assert!(
                e.contains(key) || e.contains("forbidden") || e.contains("unknown"),
                "nested {key}: {e}"
            );
        }
    }
}

/// `install_id`/`context_id`/`actor`/`by`/`digest`/`revision` are the
/// names a hostile package most wants to forge; call them out by name
/// so a silent grammar change fails loudly.
#[test]
fn refuses_each_named_authority_key_on_an_action() {
    for key in [
        "install_id",
        "context_id",
        "actor",
        "by",
        "digest",
        "revision",
        "approval_pin",
        "revision_pin",
        "guard",
        "caller",
    ] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), json!("x"));
        parse_err(&raw);
    }
}

/// A package can never weaken the host's guard floor. Optional
/// `revision_pin`/`approval_pin` booleans, an `actor` override, or a
/// `public`/`unauthenticated` flag are all authority claims — refused,
/// never "optional."
#[test]
fn refuses_guard_weakening_flags() {
    for (key, val) in [
        ("revision_pin", json!(false)),
        ("revision_pin", json!(true)),
        ("approval_pin", json!(false)),
        ("public", json!(true)),
        ("unauthenticated", json!(true)),
        ("actor", json!("operator")),
        ("actor_request", json!("public-token")),
        ("skip_approval", json!(true)),
    ] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), val);
        parse_err(&raw);
    }
}

/// Operation ids are a closed set in v1 — `record.create` and
/// `record.update` only. `record.delete`, a `custom`/arbitrary verb,
/// `send`, `publish`, an arbitrary endpoint or a SQL string all
/// refuse. An action id is never a route.
#[test]
fn refuses_unsupported_operations() {
    for op in [
        json!("record.delete"),
        json!("record.read"),
        json!("delete"),
        json!("custom"),
        json!("send"),
        json!("publish"),
        json!("POST /api/customers"),
        json!("sql:insert into customers"),
        json!("record.create "), // trailing space
        json!(" record.create"),
        json!("Record.Create"), // case
        json!(""),
        json!(1),
    ] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]["operation"] = op;
        parse_err(&raw);
    }
    let mut raw = load_example("crm.json");
    raw["actions"][0]
        .as_object_mut()
        .unwrap()
        .remove("operation");
    parse_err(&raw);
}

/// `record` names a record-kind reference in the same closed grammar —
/// not a table name, path or SQL fragment, and never an object an
/// attacker could use to carry smuggled structure.
#[test]
fn refuses_unsafe_record_references() {
    for record in [
        json!("Customer"),        // case
        json!("customer record"), // space
        json!("customers;drop"),  // injection-ish
        json!("app_records"),     // a host-internal name is not a kind
        json!("../customers"),    // path
        json!("a".repeat(65)),
        json!(""),
        json!({ "kind": "customer" }), // an object, not a ref
        json!(["customer"]),
        json!(1),
    ] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]["record"] = record;
        parse_err(&raw);
    }
    let mut raw = load_example("crm.json");
    raw["actions"][0].as_object_mut().unwrap().remove("record");
    parse_err(&raw);
}

/// Action ids are identifiers, unique within the descriptor — a
/// duplicate is a refusal (a package cannot shadow a prior action).
#[test]
fn refuses_duplicate_action_ids() {
    let mut raw = load_example("crm.json");
    let second = raw["actions"][0].clone();
    raw["actions"].as_array_mut().unwrap().push(second);
    let e = parse_err(&raw);
    assert!(e.contains("duplicate") || e.contains("action"), "{e}");
}

/// Field ids are unique within one action's input — a duplicate means
/// one input name maps to two declared types.
#[test]
fn refuses_duplicate_input_field_ids() {
    let mut raw = load_example("crm.json");
    let dup = raw["actions"][0]["input"]["fields"][0].clone();
    raw["actions"][0]["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(dup);
    let e = parse_err(&raw);
    assert!(e.contains("duplicate") || e.contains("field"), "{e}");
}

/// Field `type` is a closed vocabulary — text/number/date/datetime/
/// enum/tags — the same formats the shared host controls render. An
/// unknown type, `object`, `array`, `json`, `ref`, `file`, or a raw
/// JSON-schema keyword refuses.
#[test]
fn refuses_unknown_field_types() {
    for t in [
        json!("object"),
        json!("array"),
        json!("json"),
        json!("ref"),
        json!("file"),
        json!("blob"),
        json!("schema"),
        json!("any"),
        json!("string"), // 'string' is not the contract word — 'text' is
        json!("boolean"),
        json!(""),
        json!(1),
    ] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]["input"]["fields"][0]["type"] = t;
        parse_err(&raw);
    }
}

/// `values` is required on `enum` and forbidden on every other type —
/// an enum without a closed value list is an open-ended claim, and a
/// values list on a non-enum is contradictory.
#[test]
fn enum_values_required_and_only_on_enum() {
    // enum without values refuses.
    let mut raw = load_example("crm.json");
    raw["actions"][0]["input"]["fields"][0] = json!({
        "id": "tier", "label": "Tier", "type": "enum"
    });
    parse_err(&raw);
    // values on a non-enum refuses.
    for t in ["text", "number", "date", "datetime", "tags"] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]["input"]["fields"][0] = json!({
            "id": "x", "label": "X", "type": t, "values": ["a"]
        });
        parse_err(&raw);
    }
    // A valid enum is accepted.
    let mut raw = load_example("crm.json");
    raw["actions"][0]["input"]["fields"][0] = json!({
        "id": "tier", "label": "Tier", "type": "enum", "values": ["member", "vip"]
    });
    parse_ok(&raw);
}

/// Input fields deny unknown keys — no arbitrary JSON-schema escape,
/// no executable constraint language, no `default` that smuggles a
/// value, no `pattern`/`expression` the host would have to evaluate.
#[test]
fn refuses_unknown_field_keys() {
    for key in [
        "default",
        "pattern",
        "regex",
        "expression",
        "formula",
        "schema",
        "properties",
        "items",
        "ref",
        "endpoint",
        "script",
        "send",
        "required_roles",
        "visible_if",
    ] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]["input"]["fields"][0]
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), json!("x"));
        parse_err(&raw);
    }
}

/// String, count and size bounds: over-limit titles, labels, enum
/// values, action counts and serialized size all refuse — a descriptor
/// is a bounded review surface, not an arbitrary payload.
#[test]
fn refuses_oversize_content() {
    // Too many actions.
    let mut raw = load_example("crm.json");
    let one = raw["actions"][0].clone();
    let arr = raw["actions"].as_array_mut().unwrap();
    while arr.len() < 32 {
        let mut a = one.clone();
        a["id"] = json!(format!("a{}", arr.len()));
        arr.push(a);
    }
    parse_err(&raw);

    // Too many fields on one action.
    let mut raw = load_example("crm.json");
    let f = raw["actions"][0]["input"]["fields"][0].clone();
    let arr = raw["actions"][0]["input"]["fields"].as_array_mut().unwrap();
    while arr.len() <= 64 {
        let mut nf = f.clone();
        nf["id"] = json!(format!("f{}", arr.len()));
        arr.push(nf);
    }
    parse_err(&raw);

    // A label over its cap.
    let mut raw = load_example("crm.json");
    raw["actions"][0]["input"]["fields"][0]["label"] = json!("x".repeat(4096));
    parse_err(&raw);

    // A title over its cap.
    let mut raw = load_example("crm.json");
    raw["title"] = json!("t".repeat(4096));
    parse_err(&raw);

    // A deeply nested payload inside a non-declared object on a field —
    // the recursive scan's depth bound trips before the shape checks.
    let mut deep = json!("x");
    for _ in 0..64 {
        deep = json!({ "k": deep });
    }
    let mut raw = load_example("crm.json");
    raw["actions"][0]["input"]["fields"][0] = json!({
        "id": "x", "label": "X", "note": deep
    });
    parse_err(&raw);
}

/// The serialized-size ceiling counts bytes — a descriptor padded past
/// the cap refuses even when every individual field is in bounds. A
/// too-long `summary` trips the byte ceiling (and its own length cap).
#[test]
fn refuses_serialized_size_over_cap() {
    let mut raw = load_example("crm.json");
    raw["summary"] = json!("s".repeat(70 * 1024));
    parse_err(&raw);
}

/// Non-object descriptors, empty action lists and missing required
/// metadata all refuse — a descriptor must carry at least one action.
#[test]
fn refuses_non_object_and_empty_descriptors() {
    for raw in [
        json!(null),
        json!([]),
        json!("app-actions/v1"),
        json!(42),
        json!({ "contract": "app-actions/v1", "app": "crm", "title": "t", "actions": [] }),
        json!({ "contract": "app-actions/v1", "app": "crm", "actions": [{"id":"a","title":"t","operation":"record.create","record":"c","input":{"fields":[{"id":"f","label":"F"}]}}] }), // missing title
    ] {
        parse_err(&raw);
    }
}

/// The public-token unsubscribe path stays a separate host-custodied
/// surface — an action descriptor can never smuggle it in as a new
/// action class. A `crm_unsubscribe_redeem`-named action or a
/// `public-token`/`unauthenticated` operation is refused by the closed
/// operation grammar, not admitted.
#[test]
fn unsubscribe_is_not_an_action_class() {
    for (id, op) in [
        ("unsubscribe.redeem", "public-token"),
        ("crm_unsubscribe_redeem", "unauthenticated"),
        ("customer.unsubscribe", "unauthenticated"),
        ("send.email", "record.send"),
    ] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]["id"] = json!(id);
        raw["actions"][0]["operation"] = json!(op);
        parse_err(&raw);
    }
}

/// A well-formed `record.update` example — the second operation —
/// parses and keeps its declared shape.
#[test]
fn parses_record_update() {
    let raw = load_example("crm.json");
    let d = parse_ok(&raw);
    let update = d
        .actions
        .iter()
        .find(|a| a.operation == app_action::Operation::RecordUpdate)
        .expect("crm example declares a record.update");
    assert_eq!(update.record, "customer");
    assert!(!update.input.fields.is_empty());
}

/// Unique action ids across apps are not a constraint — the same
/// action id under a different `app` is a different descriptor. Ids
/// are only unique *within* one descriptor.
#[test]
fn action_ids_are_scoped_to_the_descriptor() {
    let mut other = load_example("ledger.json");
    // Give the ledger descriptor the same action id the crm one uses —
    // legal, because ids scope to the descriptor, not the repo.
    other["actions"][0]["id"] = json!("customer.create");
    parse_ok(&other);
}

/// Optional means absent, not null: published schema and parser agree.
#[test]
fn refuses_null_optional_properties_on_both_contract_surfaces() {
    let validator = jsonschema::validator_for(&load("app-actions.schema.json")).unwrap();
    let mut cases = Vec::new();
    let mut summary = load_example("crm.json");
    summary["summary"] = Value::Null;
    cases.push(summary);
    for key in ["type", "required", "values", "maxLength", "maxItems"] {
        let mut raw = load_example("crm.json");
        raw["actions"][0]["input"]["fields"][0][key] = Value::Null;
        cases.push(raw);
    }
    for raw in cases {
        assert!(
            !validator.is_valid(&raw),
            "schema accepted explicit null: {raw}"
        );
        parse_err(&raw);
    }
}

/// Four full identifier segments include three dots (259 characters).
#[test]
fn accepts_maximum_dotted_action_id_on_both_contract_surfaces() {
    let mut raw = load_example("crm.json");
    raw["actions"][0]["id"] = json!(vec!["a".repeat(64); 4].join("."));
    let validator = jsonschema::validator_for(&load("app-actions.schema.json")).unwrap();
    assert!(validator.is_valid(&raw));
    parse_ok(&raw);
}

/// Bound raw bytes before JSON decoding, including redundant whitespace.
#[test]
fn refuses_oversize_raw_json_before_decoding() {
    let input = format!("{}{}", " ".repeat(65536), load_example("crm.json"));
    let error = app_action::parse_str(&input).unwrap_err().to_string();
    assert!(error.contains("exceeds 65536 bytes"), "{error}");
}
