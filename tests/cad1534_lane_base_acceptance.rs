//! CAD-1534 independent acceptance check (written by the ticket author, not the
//! implementer): `issue start` must never cut a lane from the *main checkout's*
//! current branch when that branch is unrelated to the project's integration
//! ref. The invariant is deliberately mechanism-free — whether start bases the
//! lane on the integration ref or refuses outright, the foreign commit must not
//! end up in the lane.
//!
//! Reproduced twice in the field: AOS-280's lane and AOS-282's lane both came up
//! on the local-only tip `98216c4a` of `fix/board-wiki-get-read-staging`, a
//! branch that exists in no remote and is not an ancestor of `origin/staging`.
//!
//! Precondition that makes the defect possible, and which the fixture asserts:
//! `refs/remotes/origin/HEAD` is **absent**. The old behaviour fell back to the
//! main checkout's current branch in exactly that case; a fixture that sets
//! `origin/HEAD` (as `git remote set-head` does) hides the defect entirely and
//! passes both before and after the change.
//!
//! Exercises the real `issue::start::run` against an isolated PM and repo under
//! a short `/tmp` root, re-executed with HOME/XDG isolated.

use cadence_agent::issue::{
    start::{self, StartArgs},
    write, Pm,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::Builder;

const TEST: &str = "cad1534_lane_never_inherits_a_foreign_checkout_branch";
const ISOLATED: &str = "CADENCE_CAD1534_ACCEPT_ISOLATED";
const PROJECT: &str = "base";
const PREFIX: &str = "B34";
const FOREIGN: &str = "fix/foreign-local-work";

fn git_cmd(home: &Path, cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=CAD-1534 accept",
        "-c",
        "user.email=accept@invalid",
    ])
    .args(args)
    .current_dir(cwd)
    .env("HOME", home)
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
    .env_remove("GIT_DIR")
    .env_remove("GIT_WORK_TREE")
    .env_remove("GIT_INDEX_FILE");
    cmd
}

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut cmd = git_cmd(home, cwd, args);
    let out = cadence_agent::reaper::output(&mut cmd).expect("run fixture git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_ok(home: &Path, cwd: &Path, args: &[&str]) -> bool {
    let mut cmd = git_cmd(home, cwd, args);
    cadence_agent::reaper::output(&mut cmd)
        .expect("run fixture git")
        .status
        .success()
}

fn init_repo(home: &Path, repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git(home, repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join(".gitignore"), "target/\n.cadence/\n").unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    git(home, repo, &["add", ".gitignore", "tracked.txt"]);
    git(home, repo, &["commit", "--quiet", "-m", "fixture base"]);
    let origin = repo.with_file_name("repo-origin.git");
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
    // Deliberately NO `git remote set-head origin main`: the defect only exists
    // while refs/remotes/origin/HEAD is absent. Asserted in the test body.
}

fn args(repo: &Path) -> StartArgs {
    StartArgs {
        repo: Some(repo.to_path_buf()),
        name: Some("lane-b34".to_string()),
        base: None,
        owner: Some("accept".to_string()),
        job: None,
        by: Some("accept".to_string()),
        take_over: None,
    }
}

fn is_ancestor(home: &Path, repo: &Path, older: &str, newer: &str) -> bool {
    git_ok(home, repo, &["merge-base", "--is-ancestor", older, newer])
}

fn registered_worktrees(home: &Path, repo: &Path) -> Vec<PathBuf> {
    git(home, repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|p| p != repo)
        .collect()
}

#[test]
fn cad1534_lane_never_inherits_a_foreign_checkout_branch() {
    if std::env::var_os(ISOLATED).is_none() {
        let sandbox = Builder::new()
            .prefix("c1534acc-")
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
            .env_remove("CADENCE_ALIAS")
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

    let root = Builder::new().prefix("c1534r-").tempdir_in("/tmp").unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let state = root.path().join("s");
    let repo = root.path().join("repo");
    init_repo(&home, &repo);

    // The field's fixture: a checkout parked on a local-only branch whose tip is
    // in no remote and is not an ancestor of the integration ref.
    git(&home, &repo, &["checkout", "--quiet", "-b", FOREIGN]);
    fs::write(repo.join("foreign.txt"), "unmerged local work\n").unwrap();
    git(&home, &repo, &["add", "foreign.txt"]);
    git(
        &home,
        &repo,
        &["commit", "--quiet", "-m", "foreign local work"],
    );
    let foreign = git(&home, &repo, &["rev-parse", "HEAD"]);
    let integration = git(&home, &repo, &["rev-parse", "origin/main"]);
    assert_ne!(
        foreign, integration,
        "fixture must diverge from the integration ref"
    );
    assert!(
        !is_ancestor(&home, &repo, &foreign, &integration),
        "fixture foreign tip must not be an ancestor of the integration ref"
    );
    assert!(
        !git_ok(
            &home,
            &repo,
            &["rev-parse", "--verify", "refs/remotes/origin/HEAD"]
        ),
        "precondition: origin/HEAD must be absent, or the old fallback never fires and this check proves nothing"
    );

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
        "lane base fixture",
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        Some("B34-1"),
        Some("The checkout sits on a local-only branch; the lane must not inherit it."),
        "fixture-author",
    )
    .expect("fixture issue");

    let result = start::run(&pm, "B34-1", &args(&repo), "accept", &state);

    match result {
        Ok(_) => {
            let lanes = registered_worktrees(&home, &repo);
            assert_eq!(lanes.len(), 1, "expected exactly one lane, got {lanes:?}");
            let lane = &lanes[0];
            let lane_head = git(&home, lane, &["rev-parse", "HEAD"]);
            assert!(
                !is_ancestor(&home, lane, &foreign, &lane_head),
                "lane head {lane_head} carries the foreign commit {foreign} from the parked checkout"
            );
            assert!(
                is_ancestor(&home, lane, &integration, &lane_head),
                "lane head {lane_head} is not descended from the integration ref {integration}"
            );
        }
        Err(err) => {
            // Refusing is acceptable behaviour for this ticket, provided it says
            // why and leaves no lane behind.
            let text = err.to_string();
            assert!(!text.trim().is_empty(), "a refusal must explain itself");
            assert!(
                registered_worktrees(&home, &repo).is_empty(),
                "a refused start must not leave a lane behind"
            );
        }
    }
}
