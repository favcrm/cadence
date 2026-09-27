//! Git metadata fixtures exercise the actual build-script watcher without
//! nested Cargo builds, process-wide cwd changes or environment mutation.
#![allow(clippy::disallowed_methods)] // Fixture Git children are not daemon processes.

#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

struct Checkout {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Checkout {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let this = Self { _temp: temp, root };
        this.run(&["init", "--initial-branch=main"]);
        this.commit("initial");
        this
    }

    fn run(&self, args: &[&str]) -> String {
        git_at(&self.root, args)
    }

    fn commit(&self, message: &str) -> String {
        self.run(&["commit", "--allow-empty", "-m", message]);
        self.run(&["rev-parse", "HEAD"])
    }
}

fn git_at(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn watches(root: &Path) -> Vec<PathBuf> {
    let paths = build_script::checkout_watch_paths(root);
    assert!(!paths.is_empty());
    assert!(paths.iter().all(|p| p.exists()), "missing watch: {paths:?}");
    paths
}

// Record directory mtimes too: Git ref creation/deletion changes the parent.
// This checks the actual emitted paths' coverage, not Cargo fingerprints.
fn snapshot(paths: &[PathBuf]) -> Vec<(PathBuf, Option<SystemTime>)> {
    fn visit(path: &Path, result: &mut Vec<(PathBuf, Option<SystemTime>)>) {
        let Ok(meta) = std::fs::metadata(path) else {
            result.push((path.to_path_buf(), None));
            return;
        };
        result.push((path.to_path_buf(), Some(meta.modified().unwrap())));
        if meta.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                visit(&entry.unwrap().path(), result);
            }
        }
    }
    let mut result = Vec::new();
    for path in paths {
        visit(path, &mut result);
    }
    result.sort();
    result
}

fn assert_changed(paths: &[PathBuf], before: &[(PathBuf, Option<SystemTime>)]) {
    assert_ne!(
        snapshot(paths),
        before,
        "transition escaped prior watch set"
    );
}

#[test]
fn loose_branch_has_no_missing_watch_and_unchanged_reads_are_stable() {
    let repo = Checkout::new();
    let paths = watches(&repo.root);
    assert!(paths.contains(&repo.root.join(".git/refs/heads")));
    assert!(!paths.contains(&repo.root.join(".git")));
    assert!(!repo.root.join(".git/packed-refs").exists());
    // The former watch set necessarily included this missing file.
    assert!(!paths.contains(&repo.root.join(".git/packed-refs")));
    let before = snapshot(&paths);
    for _ in 0..3 {
        assert_eq!(watches(&repo.root), paths);
        assert_eq!(
            build_script::git(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
            repo.run(&["rev-parse", "HEAD"])
        );
    }
    assert_eq!(snapshot(&paths), before);
    let commit = repo.commit("move loose ref");
    assert_changed(&paths, &before);
    assert_eq!(
        build_script::git(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        commit
    );
}

#[test]
fn packing_unpacking_and_packed_ref_movement_remain_observable() {
    let repo = Checkout::new();
    let loose_paths = watches(&repo.root);
    let before = snapshot(&loose_paths);
    let original = repo.run(&["rev-parse", "HEAD"]);
    repo.run(&["pack-refs", "--all", "--prune"]);
    assert_changed(&loose_paths, &before);
    assert!(!repo.root.join(".git/refs/heads/main").exists());
    let packed_paths = watches(&repo.root);
    assert!(packed_paths.contains(&repo.root.join(".git/packed-refs")));
    assert!(!packed_paths.contains(&repo.root.join(".git/refs/heads/main")));
    let stable = snapshot(&packed_paths);
    assert_eq!(watches(&repo.root), packed_paths);
    assert_eq!(snapshot(&packed_paths), stable);
    let next = repo.commit("unpack through commit");
    assert_changed(&packed_paths, &stable);
    assert!(repo.root.join(".git/refs/heads/main").exists());
    assert_ne!(next, original);
    assert_eq!(
        build_script::git(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        next
    );
    repo.run(&["pack-refs", "--all", "--prune"]);
    let paths = watches(&repo.root);
    let before = snapshot(&paths);
    // A packed-ref-only update, with no loose ref written.
    let packed = repo.root.join(".git/packed-refs");
    let text = std::fs::read_to_string(&packed)
        .unwrap()
        .replace(&next, &original);
    let replacement = repo.root.join(".git/packed-refs.new");
    std::fs::write(&replacement, text).unwrap();
    std::fs::rename(replacement, packed).unwrap();
    assert_changed(&paths, &before);
    assert_eq!(
        build_script::git(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        original
    );
}

#[test]
fn creation_without_pruning_then_loose_deletion_uses_prior_directory_watch() {
    let repo = Checkout::new();
    let paths = watches(&repo.root);
    let original = repo.run(&["rev-parse", "HEAD"]);
    repo.run(&["pack-refs", "--all", "--no-prune"]);
    assert!(repo.root.join(".git/refs/heads/main").exists());
    assert_eq!(repo.run(&["rev-parse", "HEAD"]), original);
    let before = snapshot(&paths);
    // Even if Cargo never reran for packed-refs creation, deleting the loose
    // authoritative ref is visible through the ORIGINAL refs directory watch.
    std::fs::remove_file(repo.root.join(".git/refs/heads/main")).unwrap();
    assert_changed(&paths, &before);
    assert_eq!(repo.run(&["rev-parse", "HEAD"]), original);
    assert!(watches(&repo.root).contains(&repo.root.join(".git/packed-refs")));
}

#[test]
fn branch_switch_and_detached_head_movement_are_observable() {
    let repo = Checkout::new();
    let paths = watches(&repo.root);
    let before = snapshot(&paths);
    let original = repo.run(&["rev-parse", "HEAD"]);
    repo.run(&["checkout", "-b", "other"]);
    let next = repo.commit("other branch");
    assert_changed(&paths, &before);
    let paths = watches(&repo.root);
    let before = snapshot(&paths);
    repo.run(&["checkout", "--detach", &original]);
    assert_changed(&paths, &before);
    let detached = watches(&repo.root);
    assert_eq!(detached, vec![repo.root.join(".git/HEAD")]);
    let before = snapshot(&detached);
    repo.run(&["checkout", "--detach", &next]);
    assert_changed(&detached, &before);
    assert_eq!(
        build_script::git(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        next
    );
}

#[test]
fn linked_worktree_resolves_relative_gitdir_and_shared_refs() {
    let repo = Checkout::new();
    let linked = repo.root.parent().unwrap().join("linked");
    repo.run(&["worktree", "add", "-b", "linked", linked.to_str().unwrap()]);
    // Exercise a relative gitdir pointer as well as Git's relative commondir.
    std::fs::write(
        linked.join(".git"),
        "gitdir: ../repo/.git/worktrees/linked\n",
    )
    .unwrap();
    let paths = watches(&linked);
    assert!(paths.contains(&linked.join(".git")));
    assert!(paths.contains(&repo.root.join(".git/refs/heads")));
    assert!(paths.contains(&repo.root.join(".git/worktrees/linked/commondir")));
    assert!(!paths.contains(&repo.root.join(".git")));
    let before = snapshot(&paths);
    git_at(&linked, &["commit", "--allow-empty", "-m", "linked move"]);
    assert_changed(&paths, &before);
    let next = git_at(&linked, &["rev-parse", "HEAD"]);
    assert_eq!(
        build_script::git(&linked, &["rev-parse", "HEAD"]).unwrap(),
        next
    );
    let paths = watches(&linked);
    let before = snapshot(&paths);
    repo.run(&["pack-refs", "--all", "--prune"]);
    assert_changed(&paths, &before);
    let paths = watches(&linked);
    assert!(paths.contains(&repo.root.join(".git/packed-refs")));
    let before = snapshot(&paths);
    git_at(&linked, &["commit", "--allow-empty", "-m", "linked unpack"]);
    assert_changed(&paths, &before);
    watches(&linked);
}

#[test]
fn absent_git_falls_back_and_compiled_commit_matches_checkout() {
    let temp = tempfile::tempdir().unwrap();
    assert!(build_script::checkout_watch_paths(temp.path()).is_empty());
    assert_eq!(build_script::git(temp.path(), &["rev-parse", "HEAD"]), None);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let expected = build_script::git(root, &["rev-parse", "HEAD"]).unwrap();
    assert_eq!(env!("CADENCE_BUILD_COMMIT"), expected);
    for (actual, args) in [
        (
            env!("CADENCE_BUILD_TIME"),
            vec!["show", "-s", "--format=%cI", "HEAD"],
        ),
        (
            env!("CADENCE_BUILD_REMOTE"),
            vec!["config", "--get", "remote.origin.url"],
        ),
        (
            env!("CADENCE_BUILD_ROOT"),
            vec!["rev-parse", "--show-toplevel"],
        ),
    ] {
        assert_eq!(
            actual,
            build_script::git(root, &args).unwrap_or_else(|| "unknown".to_string())
        );
    }
}

#[test]
fn plain_clone_and_pruned_nested_ref_parent_keep_existing_watches() {
    let source = Checkout::new();
    let clone = source.root.parent().unwrap().join("clone");
    source.run(&[
        "clone",
        "--no-local",
        source.root.to_str().unwrap(),
        clone.to_str().unwrap(),
    ]);
    git_at(&clone, &["checkout", "-b", "topic/leaf"]);
    let nested = clone.join(".git/refs/heads/topic");
    let paths = watches(&clone);
    assert!(paths.contains(&nested));
    let before = snapshot(&paths);
    git_at(&clone, &["pack-refs", "--all", "--prune"]);
    if nested.exists() {
        std::fs::remove_dir(&nested).unwrap();
    }
    assert_changed(&paths, &before);
    let paths = watches(&clone);
    assert!(paths.contains(&clone.join(".git/refs/heads")));
    let before = snapshot(&paths);
    git_at(
        &clone,
        &["commit", "--allow-empty", "-m", "recreate nested ref"],
    );
    assert_changed(&paths, &before);
    assert!(nested.is_dir());
    watches(&clone);
}
