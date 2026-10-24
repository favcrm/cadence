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
