//! Conservative source-review census, not runtime authority or a Rust parser.
//! Every src/**/*.rs file is visited, including fixtures and separate databases.
//! Counts bind each classified file AND call class; another occurrence of an
//! already allowed class requires inventory review. Comments/string literals
//! deliberately count too: false positives require review rather than ignoring
//! a possible writer. The SQLite authorizer and transaction tests enforce the
//! actual runtime boundary; this inventory cannot establish semantic ownership.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

type Census = BTreeMap<String, BTreeMap<String, usize>>;
const RULES: &[(&str, &str)] = &[
    ("open", r"\b(?:Connection|Store)\s*::\s*open\w*\s*\("),
    (
        "transaction",
        r"\.\s*(?:unchecked_transaction|transaction(?:_with_behavior)?)\s*\(|\bTransaction\s*::\s*new_unchecked\s*\(",
    ),
    ("execute", r"\.\s*execute(?:_raw|_batch|_batch_raw)?\s*\("),
    ("prepare", r"\.\s*prepare(?:_cached)?\s*\("),
    (
        "guarded_write",
        r"\.\s*(?:write_tx(?:_raw)?|with_sealed_tx\w*|with_owner_tx|fixture_write)\s*\(",
    ),
];

fn scan(root: &Path) -> io::Result<Census> {
    fn visit(
        root: &Path,
        directory: &Path,
        rules: &[(String, regex::Regex)],
        census: &mut Census,
    ) -> io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_symlink() {
                return Err(io::Error::other(format!(
                    "census refuses symlink {}",
                    path.display()
                )));
            }
            if kind.is_dir() {
                visit(root, &path, rules, census)?;
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path)?;
                let counts: BTreeMap<_, _> = rules
                    .iter()
                    .filter_map(|(name, rule)| {
                        let count = rule.find_iter(&text).count();
                        (count > 0).then(|| (name.clone(), count))
                    })
                    .collect();
                if !counts.is_empty() {
                    let relative = path.strip_prefix(root).map_err(io::Error::other)?;
                    census.insert(relative.to_string_lossy().replace('\\', "/"), counts);
                }
            }
        }
        Ok(())
    }
    let rules: Vec<_> = RULES
        .iter()
        .map(|(name, pattern)| (name.to_string(), regex::Regex::new(pattern).unwrap()))
        .collect();
    let mut census = Census::new();
    visit(root, root, &rules, &mut census)?;
    Ok(census)
}

fn inventory(text: &str) -> Census {
    let mut census = Census::new();
    for line in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 4, "invalid writer inventory row");
        assert!(
            !fields[1].is_empty(),
            "every file requires an explicit owner classification"
        );
        let count: usize = fields[3].parse().expect("inventory count");
        assert!(count > 0);
        assert!(
            census
                .entry(fields[0].to_string())
                .or_default()
                .insert(fields[2].to_string(), count)
                .is_none(),
            "duplicate classification"
        );
    }
    census
}

fn verify(root: &Path, expected: &Census) -> io::Result<()> {
    let actual = scan(root)?;
    if actual == *expected {
        Ok(())
    } else {
        Err(io::Error::other(format!("writer census changed; review callsites and owner inventory\nexpected: {expected:?}\nactual: {actual:?}")))
    }
}

#[test]
fn census_db_writers_enumerated() {
    let expected = inventory(include_str!("writer-census.tsv"));
    verify(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &expected,
    )
    .unwrap();
}

#[test]
fn census_rejects_unclassified_writer() {
    let scratch = tempfile::TempDir::new().unwrap();
    let root = scratch.path();
    std::fs::create_dir(root.join("store")).unwrap();
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store/events.rs"))
            .unwrap();
    let accepted = root.join("store/events.rs");
    std::fs::write(&accepted, &source).unwrap();
    let expected = scan(root).unwrap();
    verify(root, &expected).unwrap();
    // Same allowed execute class, another occurrence in an accepted file.
    std::fs::write(
        &accepted,
        format!(
            "{source}\nfn extra(conn: &Connection) {{ conn.execute(\"DELETE FROM events\", []); }}"
        ),
    )
    .unwrap();
    assert!(verify(root, &expected).is_err());
    // A raw constructor in that same accepted file also requires review.
    std::fs::write(
        &accepted,
        format!("{source}\nfn raw() {{ Connection::open(\"cadence.sqlite3\"); }}"),
    )
    .unwrap();
    assert!(verify(root, &expected).is_err());
    std::fs::write(&accepted, &source).unwrap();
    std::fs::create_dir_all(root.join("outside/new/nested")).unwrap();
    std::fs::write(
        root.join("outside/new/nested/writer.rs"),
        "fn raw() { Connection::open(\"cadence.sqlite3\"); }",
    )
    .unwrap();
    assert!(verify(root, &expected).is_err());
}

#[test]
fn census_io_failure_is_not_an_empty_pass() {
    let scratch = tempfile::TempDir::new().unwrap();
    assert!(verify(&scratch.path().join("missing"), &Census::new()).is_err());
    std::fs::write(scratch.path().join("unreadable.rs"), [0xff]).unwrap();
    assert!(verify(scratch.path(), &Census::new()).is_err());
}
