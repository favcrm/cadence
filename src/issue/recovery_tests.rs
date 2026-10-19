//! CAD-1601: the recovery flow's own proofs.
//!
//! Every test uses a real crashed writer — a child process that takes
//! the real kernel lock, writes, and is then killed with SIGKILL — on a
//! real isolated tracker. No crash state is manufactured and no guard is
//! dropped: the refusal cases are the real guards refusing.

use serde_json::json;

use super::{decide, execute, review, status, Choice};
use crate::issue::git;
use crate::issue::lock_tests::{sigkill, spawn_writer, tracker};
use crate::issue::Pm;

/// The content of the index's copy of a path (the staged version).
fn index_content(pm: &Pm, path: &str) -> String {
    git(&pm.dir, &["cat-file", "blob", &format!(":{path}")]).unwrap()
}

/// The content of the committed copy of a path.
fn head_content(pm: &Pm, path: &str) -> String {
    git(&pm.dir, &["cat-file", "blob", &format!("HEAD:{path}")]).unwrap()
}

fn worktree_content(pm: &Pm, path: &str) -> String {
    std::fs::read_to_string(pm.dir.join(path)).unwrap()
}

/// CAD-1601: the review names the crashed writer's own paths, protects
/// the paths that pre-date the crash, and shows the content — not just
/// the names.
#[test]
fn a_review_names_the_leftover_and_protects_the_foreign_path() {
    let (_tmp, pm) = tracker();
    // Present before the crash: protected, and never selectable.
    std::fs::write(pm.dir.join("foreign.txt"), "pre-existing\n").unwrap();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);

    let r = review(&pm, Some("cadence issue reconcile --issue CAD-1")).unwrap();
    let leftovers: Vec<String> = r["leftovers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        leftovers.iter().any(|p| p == "crashed/issue.md"),
        "the review does not name the leftover: {r}"
    );
    assert!(
        !leftovers.iter().any(|p| p == "foreign.txt"),
        "a pre-existing path was blamed on the crash: {r}"
    );
    assert_eq!(r["foreign_count"], json!(1), "{r}");
    assert!(
        r["blocked_operation"]
            .as_str()
            .unwrap()
            .contains("reconcile"),
        "the review does not carry the blocked operation: {r}"
    );
    // The content is shown, not just the name: as text when it is text.
    let facts = r["paths"].as_array().unwrap();
    let fact = facts
        .iter()
        .find(|f| f["path"] == "crashed/issue.md")
        .expect("the leftover's facts are missing");
    assert!(
        fact["worktree"]["content"]["utf8"]
            .as_str()
            .unwrap_or_default()
            .contains("half a write"),
        "the review does not show the leftover's content: {fact}"
    );
    // And so is the content the keep would commit.
    assert!(
        fact["staged_content"]["utf8"]
            .as_str()
            .unwrap_or_default()
            .contains("half a write"),
        "the review does not show the staged content: {fact}"
    );
}

/// CAD-1601: the exact-index property. A keep commits the blob the
/// crashed writer STAGED, never the different version its working tree
/// held, and the working tree keeps that other version.
#[test]
fn a_keep_commits_the_staged_blob_not_the_working_tree() {
    let (_tmp, pm) = tracker();
    std::fs::write(pm.dir.join("tracked.txt"), b"base\n").unwrap();
    git(&pm.dir, &["add", "--", "tracked.txt"]).unwrap();
    git(
        &pm.dir,
        &[
            "-c",
            "user.name=cadence",
            "-c",
            "user.email=cadence@localhost",
            "commit",
            "-q",
            "-m",
            "base",
            "--",
            "tracked.txt",
        ],
    )
    .unwrap();
    let baseline = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();

    let (child, _) = spawn_writer(&pm, "hold_staged_unstaged", false);
    sigkill(child);
    // `git cat-file` output is trimmed, so the blob reads without its
    // trailing newline; the working tree keeps its bytes exactly.
    assert_eq!(index_content(&pm, "tracked.txt"), "staged");
    assert_eq!(worktree_content(&pm, "tracked.txt"), "working\n");

    let r = review(&pm, None).unwrap();
    let paths = vec!["tracked.txt".to_string()];
    let d = decide(&pm, &r, Choice::Keep, &paths).unwrap();
    let a = execute(&pm, &r, &d, Choice::Keep, &paths).unwrap();
    // The unstaged version is still in the working tree, so the
    // transaction is NOT wholly settled: the keep settled the staged
    // layer, and the rest of the difference is still reported.
    assert_eq!(a["resolved"], json!(false), "{a}");
    assert_eq!(a["remaining"], json!(["tracked.txt"]), "{a}");

    // The commit carries the STAGED blob.
    assert_eq!(
        head_content(&pm, "tracked.txt"),
        "staged",
        "the keep committed the working tree's content instead of the staged one"
    );
    // The working tree still holds the other version, byte for byte.
    assert_eq!(worktree_content(&pm, "tracked.txt"), "working\n");
    // HEAD advanced by exactly the recovery's commit.
    assert_ne!(git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(), baseline);
    assert_eq!(
        git(
            &pm.dir,
            &["rev-list", "--count", &format!("{baseline}..HEAD")]
        )
        .unwrap(),
        "1",
        "the recovery made more than one commit"
    );
}

/// CAD-1601: a discard restores the selected path to its committed
/// state — the index AND the working tree — and touches nothing else.
#[test]
fn a_discard_restores_the_index_and_the_working_tree() {
    let (_tmp, pm) = tracker();
    std::fs::write(pm.dir.join("tracked.txt"), b"base\n").unwrap();
    git(&pm.dir, &["add", "--", "tracked.txt"]).unwrap();
    git(
        &pm.dir,
        &[
            "-c",
            "user.name=cadence",
            "-c",
            "user.email=cadence@localhost",
            "commit",
            "-q",
            "-m",
            "base",
            "--",
            "tracked.txt",
        ],
    )
    .unwrap();
    let baseline = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();

    let (child, _) = spawn_writer(&pm, "hold_staged_unstaged", false);
    sigkill(child);

    let r = review(&pm, None).unwrap();
    let paths = vec!["tracked.txt".to_string()];
    let d = decide(&pm, &r, Choice::Discard, &paths).unwrap();
    let a = execute(&pm, &r, &d, Choice::Discard, &paths).unwrap();
    assert_eq!(a["resolved"], json!(true), "{a}");
    // The committed version is back in both the index and the worktree.
    assert_eq!(index_content(&pm, "tracked.txt"), "base");
    assert_eq!(worktree_content(&pm, "tracked.txt"), "base\n");
    // Nothing was committed: HEAD did not move.
    assert_eq!(git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(), baseline);
}

/// CAD-1601: leaving is a decision, not a mutation. Nothing changes and
/// nothing is reported as settled.
#[test]
fn a_leave_changes_nothing() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let before = git(&pm.dir, &["diff", "--cached"]).unwrap();
    let head_before = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();

    let r = review(&pm, None).unwrap();
    let paths = vec!["crashed/issue.md".to_string()];
    let d = decide(&pm, &r, Choice::Leave, &paths).unwrap();
    let a = execute(&pm, &r, &d, Choice::Leave, &paths).unwrap();
    assert_eq!(a["resolved"], json!(false), "{a}");
    assert_eq!(git(&pm.dir, &["diff", "--cached"]).unwrap(), before);
    assert_eq!(git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(), head_before);
    // The transaction is still open, and the status says so.
    let s = status(&pm, &a).unwrap();
    assert_eq!(s["tracker_resolved"], json!(false), "{s}");
    assert!(!s["remaining"].as_array().unwrap().is_empty(), "{s}");
}

/// CAD-1601: a protected path is refused, however explicit the
/// selection. The whole request refuses — never a successful subset.
#[test]
fn a_recovery_refuses_a_protected_path() {
    let (_tmp, pm) = tracker();
    std::fs::write(pm.dir.join("foreign.txt"), "pre-existing\n").unwrap();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let before = git(&pm.dir, &["diff", "--cached"]).unwrap();

    let r = review(&pm, None).unwrap();
    let e = decide(
        &pm,
        &r,
        Choice::Discard,
        &["crashed/issue.md".to_string(), "foreign.txt".to_string()],
    )
    .expect_err("a protected path must be refused");
    let text = e.to_string();
    assert!(text.contains("foreign.txt"), "{text}");
    assert!(text.contains("protected"), "{text}");
    assert_eq!(git(&pm.dir, &["diff", "--cached"]).unwrap(), before);
}

/// CAD-1601: the selection must be explicit and literal. An empty set, a
/// root, a directory and a Git pattern are all refused.
#[test]
fn a_recovery_refuses_a_broad_or_pattern_selection() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let r = review(&pm, None).unwrap();
    for bad in [
        vec![],
        vec![".".to_string()],
        vec!["crashed".to_string()],
        vec!["crashed/*.md".to_string()],
        vec!["crashed/../foreign.txt".to_string()],
        vec!["(top)crashed".to_string()],
    ] {
        let e = decide(&pm, &r, Choice::Discard, &bad)
            .err()
            .unwrap_or_else(|| panic!("{bad:?} must be refused"));
        assert!(
            e.to_string().contains("path") || e.to_string().contains("pattern"),
            "{bad:?}: {e}"
        );
    }
}

/// CAD-1601: a state that moved since the review is refused, and
/// nothing is changed.
#[test]
fn a_stale_review_is_refused_without_mutation() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let r = review(&pm, None).unwrap();
    let paths = vec!["crashed/issue.md".to_string()];
    let d = decide(&pm, &r, Choice::Discard, &paths).unwrap();
    // The state moves after the decision.
    std::fs::write(pm.dir.join("crashed/issue.md"), b"changed\n").unwrap();
    git(&pm.dir, &["add", "--", "crashed/issue.md"]).unwrap();
    let head_before = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();

    let e =
        execute(&pm, &r, &d, Choice::Discard, &paths).expect_err("a stale review must be refused");
    assert_eq!(e.kind(), "conflict", "{e}");
    assert_eq!(git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(), head_before);
}

/// CAD-1601: a Git-global hazard is not a path-local interruption, and
/// the recovery refuses it by name.
#[test]
fn a_global_hazard_refuses_the_recovery() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    std::fs::write(pm.dir.join(".git").join("index.lock"), b"").unwrap();

    let e = review(&pm, None).expect_err("a global hazard must refuse the review");
    assert!(e.to_string().contains("index.lock"), "{e}");
}

/// CAD-1601: when the leftover's working tree and its staged content
/// agree, a keep settles the transaction: nothing is left over, and the
/// tracker is free again. This is the case the confirmation may offer a
/// resume from.
#[test]
fn a_keep_of_a_settled_leftover_settles_the_transaction() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);

    let r = review(&pm, None).unwrap();
    let paths = vec!["crashed/issue.md".to_string()];
    let d = decide(&pm, &r, Choice::Keep, &paths).unwrap();
    let a = execute(&pm, &r, &d, Choice::Keep, &paths).unwrap();
    assert_eq!(a["resolved"], json!(true), "{a}");
    assert_eq!(a["remaining"], json!([]), "{a}");
    assert!(a["committed"].as_str().is_some(), "{a}");
    // The tracker is usable again: an ordinary write is admitted.
    let lock = pm
        .lock_for_paths(std::slice::from_ref(&pm.dir.join("crashed")))
        .expect("the tracker is free again after the recovery");
    drop(lock);
    // And the status says the transaction is settled.
    let s = status(&pm, &a).unwrap();
    assert_eq!(s["tracker_resolved"], json!(true), "{s}");
}

/// CAD-1601: a keep needs something STAGED. The untracked fixture
/// leaves the file in the working tree with nothing staged, so keeping
/// it would silently stage the working tree's content instead.
#[test]
fn a_keep_with_nothing_staged_is_refused() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_untracked", false);
    sigkill(child);
    let r = review(&pm, None).unwrap();
    let paths = vec!["crashed/issue.md".to_string()];
    let e = decide(&pm, &r, Choice::Keep, &paths)
        .expect_err("a keep with nothing staged must be refused");
    assert!(e.to_string().contains("no inspected staged content"), "{e}");
    // A discard of the same path is allowed: it has no staged content
    // to keep, and discarding it is a previewed effect.
    decide(&pm, &r, Choice::Discard, &paths).expect("a discard is allowed");
}

/// CAD-1601: a live writer refuses the recovery. The recovery must not
/// wait a writer out and then mutate with a confirmation made against
/// the old state.
#[test]
fn a_live_writer_refuses_the_recovery() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let r = review(&pm, None).unwrap();
    let paths = vec!["crashed/issue.md".to_string()];
    let d = decide(&pm, &r, Choice::Discard, &paths).unwrap();
    // A writer holds the tracker lock when the recovery arrives. It
    // takes a SCOPED lock, because an unscoped one is refused beside
    // the interruption — which is the guard working, not the fixture.
    let held = pm
        .lock_for_paths(std::slice::from_ref(&pm.dir.join("unrelated.txt")))
        .expect("a disjoint writer is admitted");
    let e = execute(&pm, &r, &d, Choice::Discard, &paths)
        .expect_err("a live writer must refuse the recovery");
    assert_eq!(e.kind(), "busy", "{e}");
    drop(held);
}

/// CAD-1601: the status must not offer a resume while any blocker
/// remains — not even when the selected paths were settled and a
/// Git-global hazard appeared afterwards.
#[test]
fn a_status_with_a_hazard_does_not_offer_a_resume() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let r = review(&pm, None).unwrap();
    let paths = vec!["crashed/issue.md".to_string()];
    let d = decide(&pm, &r, Choice::Keep, &paths).unwrap();
    let a = execute(&pm, &r, &d, Choice::Keep, &paths).unwrap();
    assert_eq!(a["recovery_success"], json!(true), "{a}");
    // A Git-global hazard appears after the recovery.
    std::fs::write(pm.dir.join(".git").join("index.lock"), b"").unwrap();
    let s = status(&pm, &a).unwrap();
    assert_eq!(s["tracker_resolved"], json!(false), "{s}");
    assert!(
        s["next_action"]
            .as_str()
            .unwrap()
            .contains("must not be resumed"),
        "{s}"
    );
}

/// CAD-1601: a tracker with no interrupted write has nothing to
/// recover, and says so rather than inventing a change set.
#[test]
fn a_clean_tracker_has_nothing_to_recover() {
    let (_tmp, pm) = tracker();
    let e = review(&pm, None).expect_err("a clean tracker has nothing to recover");
    assert!(e.to_string().contains("no interrupted write"), "{e}");
}
