//! Contract checks: the committed JSON Schema equals the one generated from
//! the Rust types, and every golden case in `tests/cases/*.json` evaluates to
//! its expected bytes. The same case files are the fixture a wasm host runs.
//!
//! A case is `{"input": <envelope object, or a string sent as raw bytes>,
//! "expected": <response>}`.
use serde_json::Value;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn committed_schemas_match_rust_types() {
    let repo = root().join("../..");
    for (path, generated) in crate::contracts::committed_schemas() {
        let path = repo.join(path);
        if std::env::var_os("CADENCE_CORE_WRITE_SCHEMA").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed,
            generated,
            "{} is stale; regenerate with CADENCE_CORE_WRITE_SCHEMA=1 cargo test -p cadence-core, then bun run --cwd ui gen:board-api",
            path.display()
        );
    }
}

#[test]
fn golden_cases() {
    let mut count = 0;
    for entry in std::fs::read_dir(root().join("tests/cases")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let case: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let input = match &case["input"] {
            Value::String(raw) => raw.clone().into_bytes(),
            envelope => serde_json::to_vec(envelope).unwrap(),
        };
        let actual: Value = serde_json::from_slice(&crate::evaluate(&input)).unwrap();
        assert_eq!(actual, case["expected"], "{}", path.display());
        count += 1;
    }
    assert!(count >= 10, "golden corpus went missing");
}

#[test]
fn unknown_operation_is_refused_as_an_invalid_command() {
    let request = serde_json::json!({"v": 1, "op": "app.screen.unknown", "input": {}});
    let response: Value =
        serde_json::from_slice(&crate::evaluate(&serde_json::to_vec(&request).unwrap())).unwrap();
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(response["code"], "invalid_command", "{response}");

    let known = serde_json::json!({
        "v": 1,
        "op": "app.manifest.preflight",
        "input": {"manifest": "---\nname: demo\n---\nbody\n"},
    });
    let response: Value =
        serde_json::from_slice(&crate::evaluate(&serde_json::to_vec(&known).unwrap())).unwrap();
    assert_eq!(response["ok"], true, "{response}");
}

// CAD-2081 acceptance, authored from the ticket by someone other than the implementer:
// the shared evaluator keeps `src/delivery/` human even when the trusted lists forget it,
// like the script's own prefix list does.
#[test]
fn the_gates_own_directory_needs_the_operator_even_when_the_lists_forget_it() {
    use crate::delivery_requirements::{change, classify, default_resolved, TrustedLists};
    let roles = root().join("../../docs/roles");
    let risk = std::fs::read_to_string(roles.join("risk-paths.toml")).unwrap();
    assert!(
        risk.contains("\"src/delivery/**\", "),
        "the real list names it"
    );
    let risk = risk.replace("\"src/delivery/**\", ", "");
    let one = std::fs::read_to_string(roles.join("one-review-paths.toml")).unwrap();
    let lists = TrustedLists {
        risk_paths: Some(&risk),
        one_review: Some(&one),
    };
    let needs_operator = |path: &str| {
        let changes = [change("M", "100644", "100644", path)];
        classify(&default_resolved(), &changes, 1, &lists).operator_approval
    };
    assert!(needs_operator("src/delivery/hosted_gate.rs"));
    assert!(!needs_operator("docs/guides/x.md"));
}
