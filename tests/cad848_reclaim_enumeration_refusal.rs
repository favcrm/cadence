//! CAD-848 independent acceptance check for fail-closed target reclamation.
//!
//! Run with `--features test-seam`: only the process-scan result is injected;
//! the real `reclaim::run_with_idle_test_process_use` path selects the lane,
//! applies the live-use/idle guards, and performs the cache deletion. Each
//! fixture has a private PM, repo, unmerged lane and target sentinel.

#![cfg(feature = "test-seam")]

use cadence_agent::issue::{
    reclaim::{self, ReclaimTestProcessUse},
    start::{self, StartArgs},
    write, Pm,
};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::{Builder, TempDir};

const TEST: &str = "cad848_reclaim_refuses_incomplete_enumeration";
const ISOLATED: &str = "CADENCE_CAD848_RECLAIM_ISOLATED";
const PROJECT: &str = "guard";
const PREFIX: &str = "C84";
const ISSUE: &str = "C84-1";
const ACTOR: &str = "guard-author";
const IDLE_SECS: u64 = 0;
const TARGET_SENTINEL: &str = ".cad848-reclaim-keep";
const TARGET_CONTENT: &[u8] = b"CAD848-RECLAIM-TARGET-KEEP\n";
const SOURCE_CONTENT: &[u8] = b"CAD848-UNMERGED-SOURCE-KEEP\n";

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    state: PathBuf,
    pm: Pm,
    repo: PathBuf,
}

impl Fixture {
    fn new(title: &str) -> Self {
        let root = Builder::new()
            .prefix("c848reclaim-")
            .tempdir_in("/tmp")
            .expect("isolated fixture root");
        let home = root.path().join("h");
        let state = root.path().join("s");
        let repo = root.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        init_repo(&home, &repo);

        let pm = Pm::init(&root.path().join("pm")).expect("isolated PM");
        write::project_add(
            &pm,
            PROJECT,
            PREFIX,
            &[repo.display().to_string()],
            &[],
            &[],
            None,
        )
        .expect("fixture project");
        write::new_issue(
            &pm,
            &repo,
            Some(PROJECT),
            title,
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some(ISSUE),
            Some("Isolated reclaim refusal fixture."),
            ACTOR,
        )
        .expect("fixture issue");
        Self {
            _root: root,
            home,
            state,
            pm,
            repo,
        }
    }

    fn start_unmerged_lane(&self) -> (PathBuf, String, String) {
        let result = start::run(
            &self.pm,
            ISSUE,
            &StartArgs {
                repo: Some(self.repo.clone()),
                name: Some("reclaim-guard".to_string()),
                base: None,
                owner: Some(ACTOR.to_string()),
                job: None,
                by: Some(ACTOR.to_string()),
                take_over: None,
            },
            ACTOR,
            &self.state,
        )
        .expect("real managed lane setup");
        let lane = PathBuf::from(result["worktree"].as_str().unwrap());
        let branch = result["branch"].as_str().unwrap().to_string();

        fs::write(lane.join("unmerged.txt"), SOURCE_CONTENT).unwrap();
        git(&self.home, &lane, &["add", "unmerged.txt"]);
        git(
            &self.home,
            &lane,
            &["commit", "--quiet", "-m", "unmerged fixture work"],
        );
        let base_tip = git(&self.home, &self.repo, &["rev-parse", "main"]);
        let branch_tip = git(&self.home, &lane, &["rev-parse", "HEAD"]);
        assert_ne!(branch_tip, base_tip, "fixture branch must remain unmerged");
        (lane, branch, branch_tip)
    }

    fn reclaim(&self, process_use: ReclaimTestProcessUse) -> Value {
        reclaim::run_with_idle_test_process_use(
            &self.pm,
            &self.state,
            ACTOR,
            IDLE_SECS,
            process_use,
        )
        .expect("run real target-reclaim path")
    }
}

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=CAD-848 reclaim guard",
        "-c",
        "user.email=reclaim-guard@invalid",
    ])
    .args(args)
    .current_dir(cwd)
    .env("HOME", home)
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
    .env_remove("GIT_DIR")
    .env_remove("GIT_WORK_TREE")
    .env_remove("GIT_INDEX_FILE");
    let out = cadence_agent::reaper::output(&mut cmd).expect("run fixture git");
    assert!(
        out.status.success(),
        "git {} in {} failed: {}{}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_repo(home: &Path, repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git(home, repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join(".gitignore"), "target/\n.cadence/\n").unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    git(home, repo, &["add", ".gitignore", "tracked.txt"]);
    git(home, repo, &["commit", "--quiet", "-m", "fixture base"]);

    let origin = repo.with_file_name(format!(
        "{}-origin.git",
        repo.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&origin).unwrap();
    git(
        home,
        &origin,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    git(
        home,
        repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(
        home,
        repo,
        &["push", "--quiet", "--set-upstream", "origin", "main"],
    );
    git(home, repo, &["remote", "set-head", "origin", "main"]);
}

struct RestorePermissions(PathBuf);

impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

fn target_with_sentinel(lane: &Path) -> PathBuf {
    let target = lane.join("target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join(TARGET_SENTINEL), TARGET_CONTENT).unwrap();
    target
}

fn assert_registration(fixture: &Fixture, lane: &Path, branch: &str, branch_tip: &str) {
    assert_eq!(
        git(&fixture.home, lane, &["rev-parse", "HEAD"]),
        branch_tip,
        "unmerged branch tip changed"
    );
    assert_eq!(
        git(&fixture.home, &fixture.repo, &["rev-parse", branch]),
        branch_tip,
        "unmerged branch ref changed"
    );
    assert!(
        git(
            &fixture.home,
            &fixture.repo,
            &["worktree", "list", "--porcelain"],
        )
        .lines()
        .any(|line| line == format!("worktree {}", lane.display())),
        "managed Git worktree registration was removed"
    );
}

fn refusal_reason<'a>(out: &'a Value, expected: &str) -> &'a str {
    let row = out["skipped"]
        .as_array()
        .expect("reclaim skipped rows")
        .iter()
        .find(|row| {
            row["issue"].as_str() == Some(ISSUE)
                && row["reason_code"].as_str() == Some("retained-by-safety-policy")
        })
        .expect("real reclaim guard must report the lane as retained");
    let reason = row["reason"].as_str().expect("refusal reason");
    assert!(
        reason.contains(expected),
        "wrong refusal reason: expected {expected:?}, got {reason:?}"
    );
    assert!(
        out["reclaimed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["issue"].as_str() != Some(ISSUE)),
        "refused lane also appeared as reclaimed"
    );
    reason
}

fn assert_reclaimed(out: &Value) {
    assert!(
        out["reclaimed"]
            .as_array()
            .expect("reclaim rows")
            .iter()
            .any(|row| row["issue"].as_str() == Some(ISSUE)),
        "control lane was not reclaimed through the real path: {out}"
    );
}

#[test]
fn cad848_reclaim_refuses_incomplete_enumeration() {
    if std::env::var_os(ISOLATED).is_none() {
        let sandbox = Builder::new()
            .prefix("c848reclaim-run-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = sandbox.path().join("home");
        let state = sandbox.path().join("state");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
            .env(ISOLATED, "1")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", sandbox.path().join("config"))
            .env("XDG_DATA_HOME", sandbox.path().join("data"))
            .env("XDG_STATE_HOME", &state)
            .env("XDG_CACHE_HOME", sandbox.path().join("cache"))
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        let out = cadence_agent::reaper::output(&mut cmd).unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "isolated acceptance run failed: {text}"
        );
        assert!(
            text.contains("1 passed"),
            "child did not run the check: {text}"
        );
        return;
    }

    // A synthetic incomplete process-use scan must refuse before any target
    // deletion. The branch is deliberately unmerged so the finish sweep can
    // neither remove the checkout nor authorize cleanup through merge state.
    let process_fixture = Fixture::new("incomplete process enumeration");
    let (process_lane, process_branch, process_tip) = process_fixture.start_unmerged_lane();
    let process_target = target_with_sentinel(&process_lane);
    let process_sentinel = process_target.join(TARGET_SENTINEL);
    let result = process_fixture.reclaim(ReclaimTestProcessUse::Incomplete(
        "test seam incomplete process enumeration".to_string(),
    ));
    refusal_reason(&result, "Cannot fully enumerate process cwd/open-fd use");
    assert_eq!(fs::read(&process_sentinel).unwrap(), TARGET_CONTENT);
    assert!(
        process_target.is_dir(),
        "process uncertainty deleted target"
    );
    assert_registration(
        &process_fixture,
        &process_lane,
        &process_branch,
        &process_tip,
    );
    let result = process_fixture.reclaim(ReclaimTestProcessUse::CompleteNoUse);
    assert_reclaimed(&result);
    assert!(
        !process_target.exists(),
        "clear process-scan control retained target"
    );
    assert!(
        process_lane.join("unmerged.txt").is_file(),
        "source was deleted"
    );
    assert_registration(
        &process_fixture,
        &process_lane,
        &process_branch,
        &process_tip,
    );

    // With a complete no-use scan, an unreadable source directory makes the
    // real idle walk incomplete. That error must retain the cache, source,
    // branch and Git registration instead of treating the lane as idle.
    let walk_fixture = Fixture::new("incomplete idle walk");
    let (walk_lane, walk_branch, walk_tip) = walk_fixture.start_unmerged_lane();
    let walk_target = target_with_sentinel(&walk_lane);
    let walk_sentinel = walk_target.join(TARGET_SENTINEL);
    let unreadable_source = walk_lane.join(".cad848-unreadable-source-entry");
    fs::create_dir(&unreadable_source).unwrap();
    fs::write(unreadable_source.join("keep"), b"CAD848-SOURCE-KEEP\n").unwrap();
    fs::set_permissions(&unreadable_source, fs::Permissions::from_mode(0o0)).unwrap();
    let restore_permissions = RestorePermissions(unreadable_source.clone());
    assert!(
        fs::read_dir(&unreadable_source).is_err(),
        "fixture must make source enumeration fail"
    );
    let result = walk_fixture.reclaim(ReclaimTestProcessUse::CompleteNoUse);
    drop(restore_permissions);
    let reason = refusal_reason(&result, "idle walk incomplete");
    assert!(
        reason.contains(&unreadable_source.display().to_string()),
        "idle-walk refusal did not name the incomplete directory: {reason}"
    );
    assert_eq!(fs::read(&walk_sentinel).unwrap(), TARGET_CONTENT);
    assert!(walk_target.is_dir(), "incomplete idle walk deleted target");
    assert_eq!(
        fs::read(unreadable_source.join("keep")).unwrap(),
        b"CAD848-SOURCE-KEEP\n"
    );
    assert_registration(&walk_fixture, &walk_lane, &walk_branch, &walk_tip);

    // Both scenarios would delete only the lane-local cache once their
    // incomplete evidence is removed, proving the checks guard real deletion.
    fs::remove_dir_all(&unreadable_source).unwrap();
    let result = walk_fixture.reclaim(ReclaimTestProcessUse::CompleteNoUse);
    assert_reclaimed(&result);
    assert!(!walk_target.exists(), "clear idle control retained target");
    assert!(
        walk_lane.join("unmerged.txt").is_file(),
        "source was deleted"
    );
    assert_registration(&walk_fixture, &walk_lane, &walk_branch, &walk_tip);
}
