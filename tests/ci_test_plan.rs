//! A Markdown content dependency can escape a literal filename scan.

use std::path::Path;

fn reviewed_docs(root: &Path) -> Result<usize, String> {
    let mut seen = 0;
    for entry in std::fs::read_dir(root.join("docs")).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
            continue;
        }
        let contents = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if !contents.contains("Reviewed-by:") {
            return Err(format!("{} has no reviewer", path.display()));
        }
        seen += 1;
    }
    if seen == 0 {
        return Err("no Markdown documents were checked".into());
    }
    Ok(seen)
}

#[test]
fn indirect_markdown_reader_passes_before_edit_and_fails_after() {
    let repo = tempfile::tempdir().unwrap();
    let docs = repo.path().join("docs");
    std::fs::create_dir(&docs).unwrap();
    // Assemble the name: the reader above never knows an individual path.
    let changed = docs.join(concat!("REVIEW", "-FLAKES.md"));
    std::fs::write(&changed, "Reviewed-by: operator\n").unwrap();
    assert_eq!(reviewed_docs(repo.path()), Ok(1));

    std::fs::write(&changed, "reviewer: operator\n").unwrap();
    let error = reviewed_docs(repo.path()).expect_err("the content edit must break the Rust read");
    assert!(error.contains("has no reviewer"), "{error}");
}
