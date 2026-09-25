//! CAD-505: the shared connected-platform fixture (ADR 0006 §5.6).
//!
//! Every document under `contracts/connected-platform/v1/` is loaded and
//! checked here so a broken vector or a drifting schema fails CI:
//!
//! - each schema file compiles as a draft-2020-12 schema;
//! - `fake-tool-table.json` validates against `tool-table.schema.json`;
//! - `vectors.json` validates against `vector.schema.json`, ids are
//!   unique, and every contract scenario the acceptance list names is
//!   present;
//! - each vector's tool table validates against the tool-table schema in
//!   the direction it claims (`given.table_valid` — malformed inputs must
//!   really be malformed);
//! - every `expect.record` specimen validates against
//!   `pending-effect.schema.json`, and its `state` matches the step's
//!   asserted state;
//! - every `call` step's expected `result` agrees with the contract's
//!   classification rules (C1–C3 plus the manifest pin), computed through
//!   the same `classify_call` the fake adapter exposes — vectors that
//!   contradict the contract fail here, before CAD-506's gate exists.
//!
//! Executing the vectors against the real gate is CAD-506's job; this
//! test is the fixture's own integrity gate.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cadence_agent::contract_fixture::{
    classify_call, Effect, FakePlatform, PendingDecision, PendingEffect, PendingOutcome,
    PendingPresser, ReadBack, ToolTable, Verified, FIXTURE_DIR, TOOL_TABLE_JSON,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_DIR)
}

fn load(name: &str) -> Value {
    let path = fixture_dir().join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::validator_for(schema).unwrap_or_else(|e| panic!("schema does not compile: {e}"))
}

fn errors_of(validator: &jsonschema::Validator, instance: &Value) -> Vec<String> {
    validator
        .iter_errors(instance)
        .map(|e| e.to_string())
        .collect()
}

/// The contract scenarios the CAD-505 acceptance list names — each must
/// be present as a vector id or the fixture silently under-covers.
const REQUIRED_VECTORS: &[&str] = &[
    "declared-read-executes",
    "undeclared-tool-parks-as-send",
    "unknown-effect-parks-as-send",
    "manifest-version-mismatch-parks",
    "effect-argument-ignored",
    "send-stages-executes-on-accept",
    "non-operator-accept-refused",
    "agent-decline-allowed",
    "decline-never-fires",
    "source-edit-cancels",
    "verified-false-raises-needs-you",
    "restart-reconciles-accepted-never-refires",
    "caller-deadline-ends-only-wait",
];

#[test]
fn schemas_compile() {
    for name in [
        "tool-table.schema.json",
        "pending-effect.schema.json",
        "vector.schema.json",
    ] {
        let schema = load(name);
        validator(&schema);
    }
}

#[test]
fn fake_tool_table_conforms_to_schema() {
    let schema = load("tool-table.schema.json");
    let table = load("fake-tool-table.json");
    let validator = validator(&schema);
    let errors = errors_of(&validator, &table);
    assert!(errors.is_empty(), "fake tool table invalid: {errors:?}");

    // The Rust view of the same file parses and sees the same tools.
    let parsed = ToolTable::from_json(&table).unwrap();
    assert_eq!(parsed.platform, "fixture");
    assert_eq!(parsed.tools.len(), 3);
    // And the adapter's embedded copy is this file byte-for-byte.
    let on_disk = std::fs::read_to_string(fixture_dir().join("fake-tool-table.json")).unwrap();
    assert_eq!(TOOL_TABLE_JSON, on_disk);
    assert_eq!(
        FakePlatform::standard().table().manifest_version.as_deref(),
        Some("1")
    );
}

#[test]
fn vectors_conform_to_schema_and_cover_acceptance() {
    let schema = load("vector.schema.json");
    let vectors = load("vectors.json");
    let validator = validator(&schema);
    let errors = errors_of(&validator, &vectors);
    assert!(errors.is_empty(), "vectors invalid: {errors:?}");

    let list = vectors["vectors"].as_array().unwrap();
    let ids: Vec<&str> = list.iter().map(|v| v["id"].as_str().unwrap()).collect();
    let unique: HashSet<&&str> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate vector ids");
    for required in REQUIRED_VECTORS {
        assert!(
            ids.contains(required),
            "acceptance scenario missing from vectors.json: {required}"
        );
    }
}

/// Resolve a vector's `given.tool_table`: `"default"` is the shared
/// fake table; anything else is an inline (possibly malformed) object.
fn vector_table(vector: &Value) -> (Value, ToolTable) {
    let raw = &vector["given"]["tool_table"];
    let table_json = if raw.as_str() == Some("default") {
        load("fake-tool-table.json")
    } else {
        raw.clone()
    };
    let table = ToolTable::from_json(&table_json).unwrap_or_else(|e| {
        panic!(
            "vector {} tool_table unparseable: {e}",
            vector["id"].as_str().unwrap()
        )
    });
    (table_json, table)
}

/// The manifest version the platform reports in this vector: absent key
/// reports the table's pin; explicit null reports nothing.
fn reported_version<'a>(vector: &'a Value, table: &'a ToolTable) -> Option<&'a str> {
    match vector["given"].get("reported_manifest_version") {
        Some(v) => v.as_str(),
        None => table.manifest_version.as_deref(),
    }
}

fn sha256(content: &str) -> String {
    let hex: String = Sha256::digest(content.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

#[test]
fn vector_tables_validate_as_claimed() {
    let schema = load("tool-table.schema.json");
    let validator = validator(&schema);
    let vectors = load("vectors.json");
    for vector in vectors["vectors"].as_array().unwrap() {
        let id = vector["id"].as_str().unwrap();
        let (table_json, table) = vector_table(vector);
        let claimed = vector["given"]["table_valid"].as_bool().unwrap_or(true);
        let valid = validator.is_valid(&table_json);
        assert_eq!(
            valid, claimed,
            "vector {id}: table_valid={claimed} but schema says {valid}"
        );
        // Tool names unique — the schema cannot express it, so the
        // fixture asserts it here.
        let names: HashSet<&str> = table.tools.iter().map(|t| t.tool.as_str()).collect();
        assert_eq!(
            names.len(),
            table.tools.len(),
            "vector {id}: duplicate tool names"
        );
    }
}

#[test]
fn specimen_records_conform_to_pending_effect_schema() {
    let schema = load("pending-effect.schema.json");
    let validator = validator(&schema);
    let vectors = load("vectors.json");
    let mut count = 0;
    for vector in vectors["vectors"].as_array().unwrap() {
        let id = vector["id"].as_str().unwrap();
        for (i, step) in vector["steps"].as_array().unwrap().iter().enumerate() {
            let Some(record) = step["expect"].get("record") else {
                continue;
            };
            count += 1;
            let errors = errors_of(&validator, record);
            assert!(
                errors.is_empty(),
                "vector {id} step {i}: record specimen invalid: {errors:?}"
            );
            // The specimen shows the state the step asserts.
            if let Some(state) = step["expect"].get("state") {
                if !state.is_null() {
                    assert_eq!(
                        record["state"].as_str().unwrap(),
                        state.as_str().unwrap(),
                        "vector {id} step {i}: record.state disagrees with expect.state"
                    );
                }
            }
            // A pinned source_hash must hash the declared source content —
            // the specimen's bytes are real, not decorative. The record's
            // own `input.source` names the artifact it derives from.
            if let Some(hash) = record.get("source_hash").and_then(Value::as_str) {
                let source = record["input"]["source"].as_str().unwrap_or_else(|| {
                    panic!(
                        "vector {id} step {i}: record pins source_hash but input names no source"
                    )
                });
                let content = vector["given"]["sources"][source]
                    .as_str()
                    .unwrap_or_else(|| panic!("vector {id} step {i}: record pins source `{source}` but given.sources lacks it"));
                assert_eq!(
                    hash,
                    sha256(content),
                    "vector {id} step {i}: source_hash is not sha256 of the declared source"
                );
            }
        }
    }
    assert!(count >= 3, "expected several record specimens, got {count}");
}

#[test]
fn vector_expectations_match_contract_classification() {
    let vectors = load("vectors.json");
    for vector in vectors["vectors"].as_array().unwrap() {
        let id = vector["id"].as_str().unwrap();
        let (_table_json, table) = vector_table(vector);
        let reported = reported_version(vector, &table);
        let mut seen_handles: HashSet<&str> = HashSet::new();
        for (i, step) in vector["steps"].as_array().unwrap().iter().enumerate() {
            let expect = &step["expect"];
            match step["action"].as_str().unwrap() {
                "call" => {
                    // A call step always asserts the routing it got.
                    let result = expect["result"].as_str().unwrap_or_else(|| {
                        panic!("vector {id} step {i}: call step lacks expect.result")
                    });
                    let handle = step["handle"].as_str();
                    let deduped = handle.is_some_and(|h| !seen_handles.insert(h));
                    let expected = if deduped {
                        "existing"
                    } else {
                        match classify_call(&table, reported, step["tool"].as_str().unwrap()) {
                            Effect::Read | Effect::Draft => "executed",
                            Effect::Send => "staged",
                        }
                    };
                    assert_eq!(
                        result, expected,
                        "vector {id} step {i}: expect.result={result} but contract routing gives {expected}"
                    );
                    // Routing and the row assertion agree.
                    match (result, expect.get("state")) {
                        ("executed" | "error", Some(state)) => {
                            assert!(
                                state.is_null(),
                                "vector {id} step {i}: {result} but state={state}"
                            )
                        }
                        ("staged" | "existing", Some(state)) => assert_eq!(
                            state.as_str().unwrap(),
                            "waiting",
                            "vector {id} step {i}: {result} but state={state}"
                        ),
                        _ => {}
                    }
                }
                "press" => {
                    let press = expect["press"].as_str().unwrap_or_else(|| {
                        panic!("vector {id} step {i}: press step lacks expect.press")
                    });
                    // C6: release is operator-only. A vector claiming a
                    // non-operator accept took must not exist.
                    if step["decision"].as_str() == Some("accept")
                        && step["by"]["role"].as_str() != Some("operator")
                    {
                        assert_eq!(
                            press, "refused",
                            "vector {id} step {i}: non-operator accept must be refused (C6)"
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

#[test]
fn typed_pending_effect_serializes_to_schema() {
    // The typed view in src/contract_fixture.rs must not drift from the
    // schema both repos consume.
    let schema = load("pending-effect.schema.json");
    let validator = validator(&schema);

    let waiting = PendingEffect {
        request: "req-1".to_string(),
        kind: "effect".to_string(),
        agent: "swe-505".to_string(),
        platform: "fixture".to_string(),
        account: "acct-1".to_string(),
        tool: "widgets.publish".to_string(),
        effect: "send".to_string(),
        input_summary: "widgets.publish widget=w1".to_string(),
        input: json!({"widget": "w1"}),
        preview: "publish w1 to fixture/acct-1".to_string(),
        source_hash: Some("sha256:abc".to_string()),
        label: Some("deploy".to_string()),
        effect_id: "eff-1".to_string(),
        state: "waiting".to_string(),
        close_reason: None,
        decision: None,
        outcome: None,
    };
    let waiting_json = serde_json::to_value(&waiting).unwrap();
    let errors = errors_of(&validator, &waiting_json);
    assert!(errors.is_empty(), "waiting specimen invalid: {errors:?}");

    let done = PendingEffect {
        state: "done".to_string(),
        decision: Some(PendingDecision {
            by: PendingPresser {
                member: "operator".to_string(),
                role: "operator".to_string(),
                rule: "operator-only".to_string(),
            },
            at: "2026-09-25T06:00:00Z".to_string(),
            reason: None,
        }),
        outcome: Some(PendingOutcome {
            result: Some(json!({"published": true})),
            error: None,
            verified: Verified::True,
        }),
        ..waiting
    };
    let serialized = serde_json::to_value(&done).unwrap();
    let errors = errors_of(&validator, &serialized);
    assert!(errors.is_empty(), "done specimen invalid: {errors:?}");
    // verified serializes as the contract's true, not a string.
    assert_eq!(serialized["outcome"]["verified"], json!(true));

    // And the malformed variants the schema must refuse.
    let mut bad = serialized.clone();
    bad["outcome"]["verified"] = json!("yes");
    assert!(
        !validator.is_valid(&bad),
        "verified:\"yes\" must be rejected"
    );
    let mut declined_with_outcome = serialized.clone();
    declined_with_outcome["state"] = json!("declined");
    assert!(
        !validator.is_valid(&declined_with_outcome),
        "a declined row carrying outcome must be rejected"
    );

    // waiting and closed rows never carry a decision: the closed case
    // is a cancelled-before-release row — no effective press exists.
    let mut waiting_with_decision = waiting_json.clone();
    waiting_with_decision["decision"] = serialized["decision"].clone();
    assert!(
        !validator.is_valid(&waiting_with_decision),
        "a waiting row carrying decision must be rejected"
    );
    let mut closed_with_decision = waiting_json;
    closed_with_decision["state"] = json!("closed");
    closed_with_decision["close_reason"] = json!("source_changed");
    closed_with_decision["decision"] = serialized["decision"].clone();
    assert!(
        !validator.is_valid(&closed_with_decision),
        "a closed-before-press row carrying decision must be rejected"
    );
    // and the same closed row without the decision is valid.
    closed_with_decision
        .as_object_mut()
        .unwrap()
        .remove("decision");
    assert!(
        validator.is_valid(&closed_with_decision),
        "a closed row with close_reason and no decision must validate"
    );
}

#[test]
fn fake_adapter_drives_the_contract_surface() {
    // The adapter the vectors describe: fixture table, deterministic
    // platform, no traffic until execute, idempotent keys, read-back.
    let adapter = FakePlatform::standard();
    assert_eq!(adapter.execution_count(), 0);

    // A read executes; the platform saw it once.
    let out = adapter.execute("widgets.list", &json!({"limit": 5}), "call-1", None);
    assert!(out.is_ok());
    assert_eq!(adapter.executions_of("widgets.list"), 1);

    // A send the gate fires executes once under its effect_id key —
    // a retried press with the same key returns the recorded outcome.
    let input = json!({"widget": "w1"});
    let first = adapter
        .execute("widgets.publish", &input, "eff-deploy-w1", None)
        .unwrap();
    let again = adapter
        .execute("widgets.publish", &input, "eff-deploy-w1", None)
        .unwrap();
    assert_eq!(first, again);
    assert_eq!(adapter.executions_of("widgets.publish"), 1);
    assert_eq!(adapter.read_back("widgets.publish", &input), Verified::True);

    // A declined effect simply never reaches execute — nothing to prove
    // but the empty log, which is exactly the point.
    assert_eq!(adapter.executions_of("widgets.preview"), 0);
}

#[test]
fn fake_adapter_models_the_vector_knobs() {
    // given.adapter.fail → the platform call errors (a `failed` outcome).
    let adapter = FakePlatform::standard();
    adapter.fail_tool("widgets.publish", "platform rejected the deploy");
    assert!(adapter
        .execute("widgets.publish", &json!({"widget": "w1"}), "eff-1", None)
        .is_err());

    // given.adapter.read_back = "unknown" — a platform with no read-back.
    adapter.set_read_back(ReadBack::Unknown);
    assert_eq!(
        adapter.read_back("widgets.publish", &json!({"widget": "w1"})),
        Verified::Unknown
    );

    // given.sources + edit_source: the hash a row pinned no longer
    // matches the artifact → source_changed.
    adapter.write_source("deploy-plan", "v1");
    let pinned = adapter.source_hash("deploy-plan").unwrap();
    adapter.write_source("deploy-plan", "v2");
    assert_ne!(adapter.source_hash("deploy-plan").unwrap(), pinned);
}
