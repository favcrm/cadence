//! Contract checks: the committed JSON Schema equals the one generated from
//! the Rust types, and every golden case in `tests/cases/*.json` evaluates to
//! its expected bytes. The same case files are the fixture a wasm host runs.
//!
//! A case is `{"input": <envelope object, or a string sent as raw bytes>,
//! "expected": <response>}`.
use crate::evaluate::{Refused, Request, Success};
use crate::issue::IssueCard;
use serde_json::Value;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn pretty(schema: schemars::Schema) -> String {
    serde_json::to_string_pretty(&schema).unwrap() + "\n"
}

#[test]
fn committed_schema_matches_rust_types() {
    let dir = root().join("../../contracts/cadence-core/v1");
    let files = [
        (
            "request.schema.json",
            pretty(schemars::schema_for!(Request)),
        ),
        (
            "success.schema.json",
            pretty(schemars::schema_for!(Success)),
        ),
        (
            "refused.schema.json",
            pretty(schemars::schema_for!(Refused)),
        ),
        (
            "issue-card.schema.json",
            pretty(schemars::schema_for!(IssueCard)),
        ),
    ];
    for (name, generated) in files {
        let path = dir.join(name);
        if std::env::var_os("CADENCE_CORE_WRITE_SCHEMA").is_some() {
            std::fs::write(&path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "{name} is stale; regenerate with CADENCE_CORE_WRITE_SCHEMA=1 cargo test -p cadence-core"
        );
    }
}

#[test]
fn committed_board_api_schema_matches_rust_types() {
    use crate::board_api::{AgentsResponse, IssuesResponse, MetaResponse};
    let dir = root().join("../../contracts/board-api/v1");
    let files = [
        (
            "issues.schema.json",
            pretty(schemars::schema_for!(IssuesResponse)),
        ),
        (
            "agents.schema.json",
            pretty(schemars::schema_for!(AgentsResponse)),
        ),
        (
            "meta.schema.json",
            pretty(schemars::schema_for!(MetaResponse)),
        ),
    ];
    for (name, generated) in files {
        let path = dir.join(name);
        if std::env::var_os("CADENCE_CORE_WRITE_SCHEMA").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "{name} is stale; regenerate with CADENCE_CORE_WRITE_SCHEMA=1 cargo test -p cadence-core, then bun run --cwd ui gen:board-api"
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
