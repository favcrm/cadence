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

/// The line's code with `//` comments stripped — but only when the
/// `//` is outside a string literal. A `//` inside `"…"` or
/// `r#"…"#`/`r##"…"##` is literal text, and a `//` inside a `'`
/// char-literal is impossible (a char literal holds one char), so only
/// string state is tracked. `"` escapes are skipped so a trailing
/// `\"` does not end the literal early.
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // A plain string literal: skip to its closing quote.
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2, // escaped char, incl. \"
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'r' if bytes.get(i + 1) == Some(&b'#') || bytes.get(i + 1) == Some(&b'"') => {
                // Raw string r"…" / r#"…"# / r##"…"##: count the `#`s and
                // find the matching `"` + `#`s terminator.
                let mut j = i + 1;
                while bytes.get(j) == Some(&b'#') {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'"') {
                    let hashes = j - (i + 1);
                    let end = j + 1;
                    // Scan for `"` followed by `hashes` `#`s.
                    let mut k = end;
                    while k < bytes.len() {
                        if bytes[k] == b'"'
                            && (0..hashes).all(|h| bytes.get(k + 1 + h) == Some(&b'#'))
                        {
                            i = k + 1 + hashes;
                            break;
                        }
                        k += 1;
                    }
                    if k >= bytes.len() {
                        i = bytes.len(); // unterminated raw string
                    }
                } else {
                    // `r` not starting a raw string (identifier char).
                    i += 1;
                }
            }
            b'\'' => {
                // Char or lifetime. A `'…'` char literal is one char;
                // skip it (and its escape) so a `'/'` can't pair into
                // `//`. Lifetimes (`'a`) never hold a `/`.
                i += 1;
                if bytes.get(i) == Some(&b'\\') {
                    i += 2;
                }
                // advance past the single char literal (if any)
                while i < bytes.len() && bytes[i] != b'\'' && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                return &line[..i];
            }
            _ => i += 1,
        }
    }
    line
}

/// Whether the `#[cfg(test)]` at `lines[idx]` gates a block (`mod` or
/// `fn`) rather than a single item (`use`/`struct`/`const`/`impl`/
/// `thread_local!`). Block-gating attributes swallow their whole body;
/// item attributes must not, or they swallow following code (the
/// `#[cfg(test)] use …;` evasion).
fn cfg_test_opens_block(lines: &[&str], idx: usize) -> bool {
    // Look at the next non-blank line after the attribute (the item it
    // annotates).
    for line in lines.iter().take(lines.len().min(idx + 6)).skip(idx + 1) {
        let item = line.trim_start();
        if item.is_empty() {
            continue;
        }
        // `mod`/`fn` items open a `{` body (a `mod x;` declaration still
        // needs no skip — it has no body here). Anything else (`use`,
        // `struct`, `const`, `impl`, `thread_local!`, …) is a single item
        // and must not start the skip.
        let is_mod_or_fn = item.starts_with("mod ")
            || item.starts_with("pub mod ")
            || item.starts_with("pub(crate) mod ")
            || item.starts_with("pub(super) mod ")
            || item.starts_with("fn ")
            || item.starts_with("pub fn ")
            || item.starts_with("pub(crate) fn ")
            || item.starts_with("pub(super) fn ")
            || item.starts_with("async fn ")
            || item.starts_with("pub async fn ")
            || item.starts_with("pub(crate) async fn ");
        // Only `mod`/`fn` with a `{` on the same line actually opens a
        // block here; a `mod x;` declaration has no inline body.
        return is_mod_or_fn && item.contains('{');
    }
    false
}

/// Production lines of `file`: `#[cfg(test)]` blocks and `//` comments
/// are test/documentation code that never ships a launch. A
/// `#[cfg(test)]` only swallows the following lines when it gates a
/// `mod`/`fn` block — on a `use`/`struct`/`const`/`impl` it annotates
/// one item and leaves the rest of the file visible to the scan.
/// Comments are stripped only outside string literals.
/// The `#[cfg(test)]`/`//`-stripped form of `text` — the production
/// view of a source file, split out so the evasion test can feed it
/// synthetic source.
fn production_source(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    let mut skip_depth: Option<i32> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(depth) = skip_depth.as_mut() {
            // Inside a `#[cfg(test)]` block: consume until it closes.
            *depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if *depth <= 0 {
                skip_depth = None;
            }
            continue;
        }
        if line.contains("#[cfg(test)]") && cfg_test_opens_block(&lines, i) {
            // Enter the block; the `{` on the item line sets depth.
            skip_depth = Some(0);
            continue;
        }
        out.push_str(strip_line_comment(line));
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
/// their spawns are the suite's own fixtures. `production_source`
/// keeps only production lines — `#[cfg(test)]` mod/fn blocks and `//`
/// comments outside string literals — so a launch after a `#[cfg(test)]
/// use` or after a `//`-in-string still reaches the needles.
///
/// Mutations: an unlisted `Command::new("boop")` in `src/` fails naming
/// the file; dropping a `resolve_on_path` provider (claude/devin/
/// cursor-agent) from the allowlist fails its pty site. The two r1
/// evasions each have a regression in
/// `the_scan_is_not_evadable_by_cfg_test_items_or_string_comments`.
/// Scan `source`'s production text for fixed-name launches. `source`
/// is the raw file text — `production_source` strips `#[cfg(test)]`
/// blocks and `//` comments first, so the same pipeline the real scan
/// runs is what the evasion test exercises.
fn scan_source(source: &str, file: &Path) -> (Vec<String>, BTreeSet<String>) {
    const NEEDLES: [&str; 4] = [
        "Command::new(",
        "resolve_on_path(",
        "command_version(",
        "probe_bins: &[(",
    ];
    let allowed: BTreeSet<&str> = FIXED_NAME_ALLOWLIST.iter().map(|(name, _)| *name).collect();
    let source = production_source(source);
    let mut offenders = Vec::new();
    let mut hits = BTreeSet::new();
    for needle in NEEDLES {
        let mut rest = source.as_str();
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
                // Not a literal program — resume after the needle so a
                // `Command::new(&var)` doesn't wedge the walk.
                None => rest = &rest[at + 1..],
            }
        }
    }
    (offenders, hits)
}

#[test]
fn every_fixed_name_program_in_src_is_allowlisted() {
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
        let raw = std::fs::read_to_string(&file).unwrap();
        let (off, hit) = scan_source(&raw, &file);
        offenders.extend(off);
        hits.extend(hit);
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

/// CAD-985 (r1): the scan must not be evadable. A `#[cfg(test)]`
/// attribute on a non-`mod`/`fn` item (`use`, `struct`, `const`,
/// `impl`, `thread_local!`) swallows only that item — a fixed-name
/// launch right after it is still seen. And a `//` inside a string
/// literal does not end the line, so a launch after `"//…"` is still
/// seen. Both were real evasions in the first version of the scan.
#[test]
fn the_scan_is_not_evadable_by_cfg_test_items_or_string_comments() {
    let dummy = Path::new("scan.rs");

    // A `#[cfg(test)] use` (or any non-block item) does not hide the
    // launch on the next line — real `src/` has `#[cfg(test)] use`s
    // (e.g. src/overview.rs:21).
    let after_item = concat!(
        "use std::process::Command;\n",
        "#[cfg(test)]\n",
        "use std::process::Stdio;\n",
        "fn probe() { let _ = Command::new(\"zzzunlistedprog\"); }\n",
    );
    let (off, _) = scan_source(after_item, dummy);
    assert!(
        off.iter().any(|o| o.contains("zzzunlistedprog")),
        "a launch after `#[cfg(test)] use` evaded the scan: {off:?}"
    );

    // A `//` inside a string literal is code, not a comment: the
    // `Command::new` after it on the same line is scanned.
    let after_str_comment = concat!(
        "fn probe() {\n",
        "    let _s = \"// not a comment\"; Command::new(\"zzzunlistedprog\");\n",
        "}\n",
    );
    let (off, _) = scan_source(after_str_comment, dummy);
    assert!(
        off.iter().any(|o| o.contains("zzzunlistedprog")),
        "a launch after a `//`-in-string evaded the scan: {off:?}"
    );

    // Controls: a `#[cfg(test)] mod {…}` still swallows its body, and a
    // real `//` comment still ends the line.
    let in_mod = concat!(
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    use super::*;\n",
        "    fn t() { let _ = Command::new(\"zzzunlistedprog\"); }\n",
        "}\n",
    );
    let (off, _) = scan_source(in_mod, dummy);
    assert!(
        !off.iter().any(|o| o.contains("zzzunlistedprog")),
        "a `#[cfg(test)] mod` body leaked into the scan: {off:?}"
    );
    let in_comment = concat!(
        "fn probe() {\n",
        "    // Command::new(\"zzzunlistedprog\") is a comment\n",
        "}\n",
    );
    let (off, _) = scan_source(in_comment, dummy);
    assert!(
        !off.iter().any(|o| o.contains("zzzunlistedprog")),
        "a `//` comment leaked into the scan: {off:?}"
    );
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
