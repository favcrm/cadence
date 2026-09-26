//! CAD-562: the split manifests are the test inventory — the set of
//! `#[test]` fn names in every generated binary must equal the set the
//! manifest lists for it. A test deleted in a move (rev-300's mut1 on
//! CAD-537) or one added without a manifest entry fails here instead of
//! going silent.
//!
//! CAD-621 retired the one-shot daemon/CLI generators and their manifests.
//! This guard still consumes tests/split-map*.toml and the doctor/host test
//! inventory; scripts/split-doctor-host --check also remains a CI gate.
//! See docs/SPLIT-MANIFESTS.md for the retained manifest policy.

use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `#[test]` fn names, in file order. The attribute may be followed by
/// more attributes (`#[ignore = "..."]`) before the `fn`.
fn test_fn_names(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut is_test = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("#[test") {
            is_test = true;
            continue;
        }
        if t.starts_with("#[") || t.is_empty() {
            continue;
        }
        if is_test {
            for prefix in ["fn ", "pub fn ", "async fn ", "pub async fn "] {
                if let Some(rest) = t.strip_prefix(prefix) {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        out.push(name);
                    }
                    break;
                }
            }
        }
        is_test = false;
    }
    out
}

/// Top-level item names in a Rust source, in file order — the same
/// reading `scripts/split-doctor-host` gives: column-0 declarations
/// only (nothing nested inside a fn body), `impl X`/`impl X for Y`
/// name `X` (the first identifier after `impl`, skipping generics),
/// `use` items name nothing.
fn item_names(src: &str) -> Vec<String> {
    const KINDS: &[&str] = &[
        "fn ", "struct ", "enum ", "const ", "static ", "type ", "trait ", "mod ",
    ];
    let mut out = Vec::new();
    for line in src.lines() {
        // Column-0 only: an indented `fn` is a local item inside a
        // body, not a manifest entry.
        let t = line
            .strip_prefix("pub(crate) ")
            .or_else(|| line.strip_prefix("pub "))
            .unwrap_or(line);
        if t.starts_with("impl ") || t.starts_with("unsafe impl ") {
            let rest = t.split_once("impl ").map(|(_, r)| r).unwrap_or("");
            let rest = rest
                .strip_prefix('<')
                .and_then(|r| r.split_once('>').map(|(_, r)| r.trim_start()))
                .unwrap_or(rest);
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                out.push(name);
            }
            continue;
        }
        for kind in KINDS {
            if let Some(r) = t.strip_prefix(kind) {
                let name: String = r
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    out.push(name);
                }
                break;
            }
        }
    }
    out
}

/// Table → array-of-strings for `tests` or `items` under `[binaries.*]`
/// or top-level `[<stem>]` sections, as `<stem> → names`.
fn manifest_lists(map: &str, key: &str) -> Vec<(String, Vec<String>)> {
    let doc: toml::Value = toml::from_str(map).expect("manifest parses");
    let mut out = Vec::new();
    if let Some(bins) = doc.get("binaries").and_then(|b| b.as_table()) {
        for (name, sec) in bins {
            if let Some(list) = sec.get(key).and_then(|v| v.as_array()) {
                out.push((
                    name.clone(),
                    list.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                ));
            }
        }
    } else if let Some(table) = doc.as_table() {
        for (stem, sec) in table {
            if let Some(list) = sec.get(key).and_then(|v| v.as_array()) {
                out.push((
                    stem.clone(),
                    list.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                ));
            }
        }
    }
    out
}

fn sorted(v: &[String]) -> BTreeSet<String> {
    v.iter().cloned().collect()
}

fn assert_sets_equal(what: &str, want: &[String], got: &[String]) {
    let (want_s, got_s) = (sorted(want), sorted(got));
    assert_eq!(
        want_s,
        got_s,
        "{what} — manifest-only: {:?}; file-only: {:?}",
        want_s.difference(&got_s).collect::<Vec<_>>(),
        got_s.difference(&want_s).collect::<Vec<_>>(),
    );
    let mut seen = BTreeSet::new();
    let dups: Vec<_> = got.iter().filter(|n| !seen.insert(*n)).collect();
    assert!(dups.is_empty(), "{what}: duplicate test names {dups:?}");
}

/// Every `[binaries.<name>]` in `map_path` names `tests/<name>.rs`; each
/// binary's `#[test]` fns must equal that section's `tests` list.
fn check_binary_map(map_path: &str) {
    let map = read(&root().join(map_path));
    for (name, want) in manifest_lists(&map, "tests") {
        let file = root().join("tests").join(format!("{name}.rs"));
        let got = test_fn_names(&read(&file));
        assert_sets_equal(&format!("{map_path} [binaries.{name}]"), &want, &got);
    }
}

#[test]
fn split_map_tests_match_integration_binaries() {
    check_binary_map("tests/split-map.toml");
}

#[test]
fn split_map_board_tests_match_board_binaries() {
    check_binary_map("tests/split-map-board.toml");
}

/// CAD-536's `src/doctor/host/split-map.toml` `[tests]` section lists
/// every item the split placed in `tests.rs` — helpers and tests —
/// plus `test_fns`, the `#[test]` fns alone. Both hold:
///   - `items` equals the file's declared items (helpers and tests
///     together, the same equality `split-doctor-host --check` runs), and
///   - `test_fns` equals the file's `#[test]` fns — so a test stripped
///     of its `#[test]` (the name stays a listed helper), deleted, or
///     added unlisted cannot hide (rev-297's mutation on #340).
#[test]
fn doctor_host_tests_match_split_map() {
    let map = read(&root().join("src/doctor/host/split-map.toml"));
    let doc: toml::Value = toml::from_str(&map).expect("manifest parses");
    let section = |key: &str| -> Vec<String> {
        doc["tests"][key]
            .as_array()
            .unwrap_or_else(|| panic!("[tests] {key} in src/doctor/host/split-map.toml"))
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let want_items = section("items");
    let want_tests = section("test_fns");
    let file = root().join("src/doctor/host/tests.rs");
    let src = read(&file);
    // The item inventory, both ways — helpers may repeat in `items`
    // (impls carry their trait's name), so compare sets.
    let (want_s, items_s) = (sorted(&want_items), sorted(&item_names(&src)));
    assert_eq!(
        want_s,
        items_s,
        "{}: [tests] items vs declared items — manifest-only: {:?}; file-only: {:?}",
        file.display(),
        want_s.difference(&items_s).collect::<Vec<_>>(),
        items_s.difference(&want_s).collect::<Vec<_>>(),
    );
    // The test inventory, both ways.
    assert_sets_equal(
        "src/doctor/host/split-map.toml [tests] test_fns",
        &want_tests,
        &test_fn_names(&src),
    );
}

/// The scanner itself: a `#[test]` stripped from a fn drops it out of
/// the test inventory while the item stays declared — the two-direction
/// doctor/host check above fails on exactly this (rev-297's mutation).
#[test]
fn scanners_see_a_stripped_test_attribute() {
    // A nested `fn` inside a body is not a declared item — the scan
    // reads column-0 declarations only.
    let src = "#[test]\nfn keeps_its_mark() {\n    fn nested() {}\n    nested();\n}\nfn a_helper() {}\nstruct Thing;\nimpl Drop for Thing {\n    fn drop(&mut self) {}\n}\nfn top_level_too() {}\n";
    assert_eq!(
        test_fn_names(src),
        vec!["keeps_its_mark".to_string()],
        "only the #[test] fn"
    );
    assert_eq!(
        sorted(&item_names(src)),
        sorted(&[
            "keeps_its_mark".to_string(),
            "a_helper".to_string(),
            "Thing".to_string(),
            "Drop".to_string(),
            "top_level_too".to_string(),
        ]),
        "every declared item, incl. impl-trait name"
    );
    let stripped = src.replacen("#[test]\n", "", 1);
    assert!(
        test_fn_names(&stripped).is_empty(),
        "a stripped #[test] leaves no test fn"
    );
    assert_eq!(
        sorted(&item_names(&stripped)),
        sorted(&item_names(src)),
        "the item survives as a helper — only test_fns catches it"
    );
}
