//! CAD-2178 acceptance: `merge_rule` accepts Cadence's normal squash merge
//! and refuses the shapes that would lose work. Real temp repos.

use super::*;

fn repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let r = tmp.path();
    git(r, &["init", "-b", "main"]).unwrap();
    git(r, &["config", "user.email", "t@t"]).unwrap();
    git(r, &["config", "user.name", "t"]).unwrap();
    std::fs::write(r.join("f"), "base\n").unwrap();
    git(r, &["add", "f"]).unwrap();
    git(r, &["commit", "-m", "base"]).unwrap();
    tmp
}

#[test]
fn multi_commit_squash_is_merged_by_patch_and_repeated_line_is_not() {
    let tmp = repo();
    let r = tmp.path();
    git(r, &["switch", "-c", "side"]).unwrap();
    for n in ["a", "b", "c"] {
        std::fs::write(r.join(n), format!("{n}\n")).unwrap();
        git(r, &["add", n]).unwrap();
        git(r, &["commit", "-m", n]).unwrap();
    }
    let tip = git(r, &["rev-parse", "HEAD"]).unwrap();
    git(r, &["switch", "main"]).unwrap();
    git(r, &["merge", "--squash", "side"]).unwrap();
    git(r, &["commit", "-m", "squashed"]).unwrap();
    assert_eq!(merge_rule(r, "side", &tip, "main"), Some("patch"));

    // Not merged: identical rows added to a long run of repeated lines.
    std::fs::write(r.join("rows"), "x\n".repeat(9)).unwrap();
    git(r, &["add", "rows"]).unwrap();
    git(r, &["commit", "-m", "rows"]).unwrap();
    git(r, &["switch", "-c", "rows", "main"]).unwrap();
    std::fs::write(r.join("rows"), "x\n".repeat(10)).unwrap();
    git(r, &["commit", "-am", "more"]).unwrap();
    let tip = git(r, &["rev-parse", "HEAD"]).unwrap();
    git(r, &["switch", "main"]).unwrap();
    assert_eq!(merge_rule(r, "rows", &tip, "main"), None);
}
