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

// CAD-2386 (operator decision 2026-11-17) supersedes CAD-2081's in-code floor for
// `src/delivery/`: product code names no repository's paths. The trusted lists
// protect themselves instead. Editing them is operator-class, so dropping a gate
// path from them needs an operator-approved change, and a list that loses a
// required table fails closed.
#[test]
fn the_trusted_lists_protect_themselves() {
    use crate::delivery_requirements::{change, classify, default_resolved, TrustedLists};
    let roles = root().join("../../docs/roles");
    let risk = std::fs::read_to_string(roles.join("risk-paths.toml")).unwrap();
    let one = std::fs::read_to_string(roles.join("one-review-paths.toml")).unwrap();
    let requirements = |risk: &str, path: &str| {
        let lists = TrustedLists {
            risk_paths: Some(risk),
            one_review: Some(&one),
        };
        let changes = [change("M", "100644", "100644", path)];
        classify(&default_resolved(), &changes, 1, &lists)
    };
    // The lists and the gate's own directory need the operator under the real lists.
    assert!(requirements(&risk, "docs/roles/risk-paths.toml").operator_approval);
    assert!(requirements(&risk, "docs/roles/one-review-paths.toml").operator_approval);
    assert!(requirements(&risk, "src/delivery/hosted_gate.rs").operator_approval);
    assert!(!requirements(&risk, "docs/guides/x.md").operator_approval);
    // A list missing any one required table fails closed: every path needs
    // the operator and two reviews. Each table is renamed on its own, so
    // making any single one optional fails this test.
    for name in ["operator", "browser_qa", "routine"] {
        let header = format!("\n[{name}]\n");
        assert!(risk.contains(&header), "the real list has a {name} table");
        let cut = risk.replace(&header, &format!("\n[{name}_renamed]\n"));
        let r = requirements(&cut, "docs/guides/x.md");
        assert!(r.operator_approval, "{name} missing must need the operator");
        assert_eq!(r.reviews, 2, "{name} missing must keep two reviews");
    }
}
