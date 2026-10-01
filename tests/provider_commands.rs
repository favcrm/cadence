//! CAD-968: every provider launch command defaults to `false` in the test
//! env, so a test that forgets a mock fails instead of launching a real,
//! credentialed provider (or a real tmux server).

// `common` carries fixture spawns; a test binary never runs the CAD-308
// reaper (only `daemon run` does).
#![allow(clippy::disallowed_methods)]

mod common;
use common::*;

use cadence_agent::adapter::{ProviderEnv, PROVIDER_COMMAND_VARS, REFUSED_COMMAND};
use std::collections::BTreeSet;
use std::path::Path;

/// Written out by hand, not derived from the const under test: a name
/// dropped from `PROVIDER_COMMAND_VARS` must fail here.
const EXPECTED: [&str; 9] = [
    "CADENCE_CLAUDE_COMMAND",
    "CADENCE_CLAUDE_TUI_COMMAND",
    "CADENCE_CODEX_COMMAND",
    "CADENCE_CODEX_WS_COMMAND",
    "CADENCE_CODEX_SANDBOX_COMMAND",
    "CADENCE_DEVIN_COMMAND",
    "CADENCE_CURSOR_COMMAND",
    "CADENCE_PI_COMMAND",
    "CADENCE_TMUX_COMMAND",
];

/// `CADENCE_*_COMMAND` names the source reads that are not a provider's
/// launch command: the confine/MCP helpers and the test-only stub TUI.
const NOT_PROVIDERS: [&str; 3] = [
    "CADENCE_CONFINE_COMMAND",
    "CADENCE_MCP_PERMISSION_COMMAND",
    "CADENCE_STUB_COMMAND",
];

#[test]
fn the_provider_list_is_the_one_the_ticket_names() {
    let listed: BTreeSet<&str> = PROVIDER_COMMAND_VARS.into_iter().collect();
    let expected: BTreeSet<&str> = EXPECTED.into_iter().collect();
    assert_eq!(listed, expected);
}

#[test]
fn every_provider_command_is_false_in_the_test_env_and_for_subprocesses() {
    let vars: std::collections::BTreeMap<String, String> = test_env().vars().into_iter().collect();
    for name in EXPECTED {
        assert_eq!(
            test_env().var(name).as_deref(),
            Some(REFUSED_COMMAND),
            "{name} must refuse in the test env"
        );
        assert_eq!(
            vars.get(name).map(String::as_str),
            Some("false"),
            "{name} must reach subprocess daemons through vars()"
        );
    }
}

#[test]
fn a_daemon_built_from_the_refusing_env_refuses_every_provider() {
    let env = ProviderEnv::refusing_providers();
    for name in EXPECTED {
        assert_eq!(env.own(name).as_deref(), Some("false"), "{name}");
    }
}

#[test]
fn the_plain_default_env_is_not_what_tests_use() {
    // The control: an empty env falls through to the real binary, which is
    // exactly what the refusing env exists to prevent.
    let env = ProviderEnv::default();
    for name in EXPECTED {
        assert_eq!(env.own(name), None, "{name}");
    }
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A new `CADENCE_<X>_COMMAND` read in `src/` must be listed as a provider
/// or named as a non-provider here, or a test that forgets its mock would
/// reach the real thing.
#[test]
fn every_command_variable_in_src_is_listed_or_named_as_not_a_provider() {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut seen = BTreeSet::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let mut rest = text.as_str();
        while let Some(at) = rest.find("\"CADENCE_") {
            let tail = &rest[at + 1..];
            let end = tail.find('"').unwrap_or(tail.len());
            let name = &tail[..end];
            if name.ends_with("_COMMAND")
                && name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            {
                seen.insert(name.to_string());
            }
            rest = &tail[end..];
        }
    }
    assert!(
        seen.len() >= EXPECTED.len(),
        "scan found too little: {seen:?}"
    );
    for name in &seen {
        assert!(
            EXPECTED.contains(&name.as_str()) || NOT_PROVIDERS.contains(&name.as_str()),
            "{name} is read in src/ but is neither a listed provider command nor a named \
             non-provider: add it to PROVIDER_COMMAND_VARS (src/adapter/mod.rs)"
        );
    }
    for name in EXPECTED {
        assert!(
            seen.contains(name),
            "{name} is listed but src/ never reads it"
        );
    }
}
