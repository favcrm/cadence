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

/// CAD-985: a program name `src/` may launch by fixed name — either a
/// provider/doctor probe that tests must stub (`cli_doctor_smoke`), or a
/// non-provider support tool. Any other launch must resolve through its
/// `CADENCE_*_COMMAND` override, which the test env points at `false`.
const FIXED_NAME_ALLOWLIST: [(&str, &str); 16] = [
    // Provider binaries — each is a registry `probe_bins` entry, and a
    // `resolve_on_path` fallback for its `CADENCE_*_COMMAND`.
    (
        "claude",
        "probe_bins entry; resolve_on_path fallback for CADENCE_CLAUDE_TUI_COMMAND",
    ),
    ("codex", "probe_bins entry"),
    ("pi", "probe_bins entry"),
    (
        "devin",
        "probe_bins entry; resolve_on_path fallback for CADENCE_DEVIN_COMMAND",
    ),
    (
        "cursor-agent",
        "probe_bins entry; resolve_on_path fallback for CADENCE_CURSOR_COMMAND",
    ),
    // Support tools, not providers — never credentialed.
    (
        "tmux",
        "probe_bins entry; attach.rs `tmux attach` view on the owned -L socket",
    ),
    ("git", "version control, not a provider"),
    ("gh", "issue reconcile/audit, not a provider"),
    ("sh", "POSIX shell for compound commands"),
    ("setsid", "session detach for lane children and agent-exec"),
    ("sleep", "bounded helper waits (client/peer/slots/rollout)"),
    ("timeout", "issue/app.rs call wrapper"),
    ("tar", "issue/history.rs archive"),
    (
        "tailscale",
        "ui.rs `tailscale serve` share, operator opted in",
    ),
    ("pdftotext", "wiki/ingest.rs PDF text extraction"),
    (
        "node",
        "cli/app.ts helper — the CAD-559 pi-devin extension runtime",
    ),
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

/// The program name of a `<ctor>( "prog" …` call — the literal must be
/// the *first* token of the argument list (only whitespace before it),
/// so `Command::new(&var).arg("x")`, `Command::new(prog)` and
/// `.args(["x"])` sites never count. The name must be a bare program
/// (lowercase ident, `-`/`_`/`.` allowed, no `/`, no space, no
/// `-`-lead); absolute paths, args and prompt glyphs never match.
fn program_literal(text: &str, at: usize) -> Option<String> {
    let tail = text[at..].trim_start();
    let close = tail.strip_prefix('"')?.find('"')?;
    let name = &tail[1..1 + close];
    let bare = !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-..".contains(c))
        && name.chars().any(|c| c.is_ascii_lowercase());
    bare.then(|| name.to_string())
}

/// Whether `file` ships launch code the scan must see — test support
/// (`*_tests.rs`, `tests.rs`, `test_queue.rs`) never does.
fn launches_production_code(file: &Path) -> bool {
    let name = file.file_name().unwrap().to_string_lossy();
    !(name.ends_with("_tests.rs") || name == "tests.rs" || name == "test_queue.rs")
}

/// Production lines of `file`: `#[cfg(test)]` modules and `//` comment
/// text are test/documentation code that never ships a launch.
fn production_text(file: &Path) -> String {
    let text = std::fs::read_to_string(file).unwrap();
    let mut out = String::new();
    let mut in_test = false;
    let mut depth = 0i32;
    for line in text.lines() {
        if line.contains("#[cfg(test)]") {
            in_test = true;
            depth = 0;
            continue;
        }
        if in_test {
            // `#[cfg(test)]` starts a `mod … {` block; leave it once its
            // braces close. Braces in strings are rare in a test mod
            // header line, and the modules are always last in the file.
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if depth < 0 {
                in_test = false;
            }
            continue;
        }
        // Keep code before a `//` comment; `//` inside a program-name
        // literal is impossible (the literal filter drops `/`).
        out.push_str(line.split("//").next().unwrap_or(""));
        out.push('\n');
    }
    out
}

/// CAD-985: every fixed-name program a non-test `src/` line launches —
/// `Command::new("<prog>")`, `resolve_on_path("<prog>")`, a
/// `probe_bins` `("<prog>", …)` tuple or a `command_version("<prog>")`
/// — must be in `FIXED_NAME_ALLOWLIST` with a reason, or resolve
/// through its `CADENCE_*_COMMAND` override. Without this scan a fresh
/// `resolve_on_path("<provider>")` or `Command::new("<provider>")`
/// runs the host's real, credentialed binary the day a test forgets
/// its mock — the gap `command_version`'s doctor probe fell through.
/// Test/support files (`*_tests.rs`, `test_queue.rs`) are excluded:
/// their spawns are the suite's own fixtures.
///
/// Mutations: an unlisted `Command::new("boop")` in `src/` fails naming
/// the file; dropping a `resolve_on_path` provider (claude/devin/
/// cursor-agent) from the allowlist fails its pty site.
#[test]
fn every_fixed_name_program_in_src_is_allowlisted() {
    const NEEDLES: [&str; 4] = [
        "Command::new(",
        "resolve_on_path(",
        "command_version(",
        "probe_bins: &[(",
    ];
    let allowed: BTreeSet<&str> = FIXED_NAME_ALLOWLIST.iter().map(|(name, _)| *name).collect();
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut offenders = Vec::new();
    let mut hits = BTreeSet::new();
    for file in files {
        if !launches_production_code(&file) {
            continue;
        }
        let text = production_text(&file);
        for needle in NEEDLES {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(needle) {
                // `probe_bins: &[("x", …), ("y", …)]` — each tuple's
                // program is the literal right after its `(`; the list
                // ends at `)]` or `&[]`.
                let after = at + needle.len();
                if needle == "probe_bins: &[(" {
                    let span_end = rest[after..].find(")]").map(|e| after + e + 2);
                    let span = span_end.map_or(&rest[after..], |e| &rest[..e]);
                    let mut tail = span;
                    while let Some(open) = tail.find("(\"") {
                        // A `("…")` that is not a program name (never
                        // happens today) still advances the walk.
                        if let Some(name) = program_literal(tail, open + 1) {
                            if allowed.contains(name.as_str()) {
                                hits.insert(name);
                            } else {
                                offenders.push(format!(
                                    "{} probe_bins `{name}` — list it in \
                                     FIXED_NAME_ALLOWLIST",
                                    file.display()
                                ));
                            }
                        }
                        tail = &tail[open + 2..];
                    }
                    rest = &rest[span_end.unwrap_or(after)..];
                    continue;
                }
                match program_literal(rest, after) {
                    Some(name) => {
                        if allowed.contains(name.as_str()) {
                            hits.insert(name);
                        } else {
                            offenders.push(format!(
                                "{} runs `{name}` via {needle} — list it in \
                                 FIXED_NAME_ALLOWLIST with a reason, or launch it through its \
                                 CADENCE_*_COMMAND override",
                                file.display()
                            ));
                        }
                        rest = &rest[after..];
                    }
                    // Not a literal program — resume after the needle
                    // so a `Command::new(&var)` doesn't wedge the walk.
                    None => rest = &rest[at + 1..],
                }
            }
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
    // The allowlist must not rot: every name in it still launches
    // somewhere in src/, or it leaves the list.
    for (name, reason) in FIXED_NAME_ALLOWLIST {
        assert!(
            hits.contains(name),
            "allowlisted `{name}` ({reason}) has no launch site left in src/"
        );
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
