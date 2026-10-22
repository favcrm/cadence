//! Pure analysis helpers for review diffs and test output.

use std::path::Path;

/// `*`/`**`/`?` glob match: `*` stays inside a path component, `**`
/// crosses directories, `?` is one non-`/` character.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    glob_rec(pattern.as_bytes(), path.as_bytes())
}

fn glob_rec(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    match p[0] {
        b'*' if p.get(1) == Some(&b'*') => {
            // `**/`: also try matching zero directories.
            if p.get(2) == Some(&b'/') && glob_rec(&p[3..], s) {
                return true;
            }
            for i in 0..=s.len() {
                if glob_rec(&p[2..], &s[i..]) {
                    return true;
                }
            }
            false
        }
        b'*' => {
            for i in 0..=s.len() {
                if glob_rec(&p[1..], &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == b'/' {
                    break;
                }
            }
            false
        }
        b'?' => !s.is_empty() && s[0] != b'/' && glob_rec(&p[1..], &s[1..]),
        c => !s.is_empty() && s[0] == c && glob_rec(&p[1..], &s[1..]),
    }
}

/// A `fn` item added under `test_globs` in the PR diff, with the added
/// lines that follow it (its body, for the stress-pattern match).
#[derive(Clone, Debug)]
pub struct NewTest {
    pub name: String,
    pub file: String,
    pub body: String,
}

/// Parse a unified diff (`git diff --unified=0 <merge-base> <head>`)
/// for `fn` items added under files matching `globs`, then keep those
/// whose name or added body contains any `patterns` substring (an
/// empty pattern list keeps every new test). A `fn` only counts as a
/// test when an added `#[test]`/`#[tokio::test]` attribute precedes it —
/// helpers like `fn review_fixture` are not tests and stressing them
/// would run zero tests.
pub fn parse_new_tests(diff: &str, globs: &[String], patterns: &[String]) -> Vec<NewTest> {
    let mut file = String::new();
    let mut file_ok = false;
    let mut tests: Vec<NewTest> = Vec::new();
    let mut cur: Option<usize> = None;
    // Contiguous added `#[...]` lines immediately before the next `fn`.
    let mut attrs: Vec<String> = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            file = rest.trim().to_string();
            file_ok = globs.iter().any(|g| glob_match(g, &file));
            cur = None;
            attrs.clear();
            continue;
        }
        if line.starts_with("+++")
            || line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("Binary")
        {
            // `+++ /dev/null` (deleted file) and every other header
            // clears the file match too.
            file.clear();
            file_ok = false;
            cur = None;
            attrs.clear();
            continue;
        }
        if !file_ok {
            continue;
        }
        if let Some(added) = line.strip_prefix('+') {
            if let Some(name) = added_fn_name(added) {
                if attrs.iter().any(|a| is_test_attr(a)) {
                    tests.push(NewTest {
                        name,
                        file: file.clone(),
                        body: String::new(),
                    });
                    cur = Some(tests.len() - 1);
                } else {
                    cur = None;
                }
                attrs.clear();
            } else {
                if let Some(i) = cur {
                    tests[i].body.push_str(added);
                    tests[i].body.push('\n');
                }
                let t = added.trim();
                if t.starts_with("#[") {
                    attrs.push(t.to_string());
                } else if !t.is_empty() {
                    attrs.clear();
                }
            }
        } else {
            // Context/removal/hunk boundary: the fn's added block ends.
            cur = None;
            attrs.clear();
        }
    }
    tests.retain(|t| {
        patterns.is_empty()
            || patterns.iter().any(|p| {
                !p.is_empty() && (t.name.contains(p.as_str()) || t.body.contains(p.as_str()))
            })
    });
    tests
}

/// `#[test]` or `#[tokio::test]` (with or without arguments).
fn is_test_attr(line: &str) -> bool {
    let t = line.trim();
    t == "#[test]" || t.starts_with("#[tokio::test")
}

/// `fn name(` out of an added diff line — accepts `fn`, `pub fn`,
/// `pub(crate) fn`, `async fn` and leading whitespace.
fn added_fn_name(added: &str) -> Option<String> {
    let t = added.trim_start();
    let t = t
        .strip_prefix("pub(crate) ")
        .or_else(|| t.strip_prefix("pub "))
        .unwrap_or(t);
    let t = t.strip_prefix("async ").unwrap_or(t);
    let rest = t.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Cargo-style failure extraction: `test <name> ... FAILED` lines plus
/// the indented `failures:` summary block.
pub fn extract_failed_tests(output: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in output.lines() {
        let t = line.trim();
        if t == "failures:" {
            in_block = true;
            continue;
        }
        if in_block {
            if t.is_empty() {
                continue;
            }
            if line.starts_with("    ") {
                names.push(t.to_string());
                continue;
            }
            in_block = false;
        }
        if let Some(rest) = t.strip_prefix("test ") {
            if let Some((name, _)) = rest.split_once(" ... FAILED") {
                names.push(name.trim().to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Added-line heuristic for a schema migration: `ALTER TABLE <name>`
/// or a bump of the store's `schema_version` table. Word-boundary aware
/// so code that merely mentions the phrases — like this detector —
/// doesn't trip it.
pub fn schema_migration_hit(diff: &str) -> Vec<String> {
    // Built at runtime so this file's own diff doesn't read as a hit.
    let sv_ops: Vec<String> = ["update", "into", "from", "exists"]
        .iter()
        .map(|op| format!("{op} schema_version"))
        .collect();
    let mut hits = Vec::new();
    for line in diff.lines() {
        let Some(added) = line.strip_prefix('+') else {
            continue;
        };
        if added.starts_with('+') {
            continue; // the +++ header line
        }
        let low = added.to_lowercase();
        if phrase_then_word(&low, "alter table")
            || set_version_bump(&low)
            || sv_ops.iter().any(|p| low.contains(p))
        {
            hits.push(added.trim().to_string());
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

/// `needle` appears followed by whitespace then an identifier char.
fn phrase_then_word(s: &str, needle: &str) -> bool {
    s.match_indices(needle).any(|(i, _)| {
        s[i + needle.len()..]
            .trim_start()
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric() || c == '_')
            .unwrap_or(false)
    })
}

/// `set version` then `=` then a digit — the store's version bump idiom.
fn set_version_bump(s: &str) -> bool {
    s.match_indices("set version").any(|(i, _)| {
        s[i + "set version".len()..]
            .trim_start()
            .strip_prefix('=')
            .and_then(|r| r.trim_start().chars().next())
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    })
}

/// The isolated-test command for one test: `{test}` `{file}` `{target}`
/// substitution. `{target}` is the cargo test target: the file stem, or
/// `suite` for a file that `tests/suite.rs` declares as a module — those
/// test names are module-qualified, so `{test}` gains the module prefix.
/// Every value is single-quoted: the names come from diff and test
/// output the PR controls, so the template must not pre-quote.
pub fn test_command(template: &str, test: &NewTest, suite: &[String]) -> String {
    let stem = Path::new(&test.file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let in_suite = !stem.is_empty() && suite.iter().any(|m| m == &stem);
    let (target, name) = if in_suite {
        let name = if test.name.contains("::") {
            test.name.clone()
        } else {
            format!("{stem}::{}", test.name)
        };
        ("suite".to_string(), name)
    } else {
        (stem, test.name.clone())
    };
    template
        .replace("{test}", &sh_quote(&name))
        .replace("{file}", &sh_quote(&test.file))
        .replace("{target}", &sh_quote(&target))
}

/// Module names `tests/suite.rs` declares with `mod <name>;`. The review
/// routes a test in one of those files through the `suite` target instead
/// of a `--test <stem>` target that no longer exists (CAD-1727).
pub fn suite_modules(dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dir.join("tests/suite.rs")) else {
        return Vec::new();
    };
    let mut mods: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("mod "))
        .filter_map(|r| r.strip_suffix(';'))
        .map(str::trim)
        .filter(|m| {
            !m.is_empty()
                && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !m.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .map(str::to_string)
        .collect();
    mods.sort();
    mods.dedup();
    mods
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
