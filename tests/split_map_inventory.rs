//! CAD-562: the split manifests are the test inventory — the set of
//! `#[test]` fn names in every generated binary must equal the set the
//! manifest lists for it. A test deleted in a move (rev-300's mut1 on
//! CAD-537) or one added without a manifest entry fails here instead of
//! going silent.

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
/// every item the split placed in `tests.rs` — helpers and tests. Every
/// `#[test]` fn in the file must be listed (no silent drops), and no
/// listed test may be missing.
#[test]
fn doctor_host_tests_match_split_map() {
    let map = read(&root().join("src/doctor/host/split-map.toml"));
    let want = manifest_lists(&map, "items")
        .into_iter()
        .find(|(stem, _)| stem == "tests")
        .map(|(_, items)| items)
        .expect("[tests] in src/doctor/host/split-map.toml");
    let file = root().join("src/doctor/host/tests.rs");
    let got = test_fn_names(&read(&file));
    let (want_s, got_s) = (sorted(&want), sorted(&got));
    assert!(
        got_s.is_subset(&want_s),
        "{}: #[test] fns not in [tests] of src/doctor/host/split-map.toml: {:?}",
        file.display(),
        got_s.difference(&want_s).collect::<Vec<_>>(),
    );
    // Every listed test-named item must be a real fn — a helper renamed
    // over a test's name would hide its loss. `items` has helpers too,
    // so compare against the file's #[test] set, not every listed name.
    let listed_tests: Vec<String> = want
        .into_iter()
        .filter(|n| got_s.contains(n) || got.iter().any(|g| g == n))
        .collect();
    assert_sets_equal(
        "src/doctor/host/split-map.toml [tests] test fns",
        &listed_tests,
        &got,
    );
}
