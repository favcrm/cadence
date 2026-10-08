//! CAD-1191 independent acceptance check (written by the reviewer, not the
//! implementer): `issue start` may set up its worktree outside the PM write
//! lock, but two concurrent starts on one issue must still produce exactly
//! one claim. The loser refuses with the CAD-383 "held by" refusal and leaves
//! no lane behind: no registered worktree, no lane directory, no lane refs.
//!
//! Exercises the real `issue::start::run` against an isolated PM and repo
//! under a short `/tmp` root, re-executed with HOME/XDG isolated.

use cadence_agent::issue::{
    parse,
    start::{self, StartArgs},
    write, Pm,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use tempfile::Builder;

const TEST: &str = "cad1191_concurrent_starts_yield_exactly_one_claim";
const ISOLATED: &str = "CADENCE_CAD1191_ACCEPT_ISOLATED";
const PROJECT: &str = "race";
const PREFIX: &str = "R11";
const ROUNDS: usize = 4;

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=CAD-1191 accept",
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
    let out = cadence_agent::reaper::output(&mut cmd).expect("run fixture git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
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
    git(home, repo, &["remote", "set-head", "origin", "main"]);
}

fn args(repo: &Path, who: &str) -> StartArgs {
    StartArgs {
        repo: Some(repo.to_path_buf()),
        name: Some(format!("lane-{who}")),
        base: None,
        owner: Some(who.to_string()),
        job: None,
        by: Some(who.to_string()),
        take_over: None,
    }
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
fn cad1191_concurrent_starts_yield_exactly_one_claim() {
    if std::env::var_os(ISOLATED).is_none() {
        let sandbox = Builder::new()
            .prefix("c1191acc-")
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

    let root = Builder::new().prefix("c1191r-").tempdir_in("/tmp").unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let state = root.path().join("s");
    let repo = root.path().join("repo");
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

    for round in 1..=ROUNDS {
        let id = format!("{PREFIX}-{round}");
        write::new_issue(
            &pm,
            &repo,
            Some(PROJECT),
            &format!("race round {round}"),
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some(&id),
            Some("Concurrent start fixture."),
            "fixture-author",
        )
        .expect("fixture issue");
        let before = registered_worktrees(&home, &repo).len();

        let gate = Arc::new(Barrier::new(2));
        let racers: Vec<_> = ["racer-a", "racer-b"]
            .into_iter()
            .map(|who| {
                let (gate, pm_dir, repo, state, id) = (
                    gate.clone(),
                    pm.dir.clone(),
                    repo.clone(),
                    state.clone(),
                    id.clone(),
                );
                std::thread::spawn(move || {
                    let pm = Pm::at(&pm_dir).unwrap();
                    gate.wait();
                    (who, start::run(&pm, &id, &args(&repo, who), who, &state))
                })
            })
            .collect();
        let results: Vec<_> = racers.into_iter().map(|t| t.join().unwrap()).collect();

        let won: Vec<_> = results.iter().filter(|(_, r)| r.is_ok()).collect();
        let lost: Vec<_> = results.iter().filter(|(_, r)| r.is_err()).collect();
        assert_eq!(
            (won.len(), lost.len()),
            (1, 1),
            "round {round}: exactly one start must win: {results:?}"
        );
        let (winner, ok) = won[0];
        let (loser, err) = lost[0];
        let refusal = err.as_ref().unwrap_err().to_string();
        assert!(
            refusal.contains("held by"),
            "round {round}: loser {loser} must get the CAD-383 claim refusal, got: {refusal}"
        );

        // The tracker records only the winner's claim and lane.
        let issue_md = pm.dir.join(PROJECT).join(&id).join("issue.md");
        let (front, _) = parse::parse_issue(&fs::read_to_string(&issue_md).unwrap()).unwrap();
        assert_eq!(
            front.claim.as_ref().map(|c| c.by.as_str()),
            Some(*winner),
            "round {round}: claim must name the winner"
        );
        let lane = PathBuf::from(ok.as_ref().unwrap()["worktree"].as_str().unwrap());
        let open: Vec<_> = front
            .refs
            .iter()
            .filter(|r| r.kind == "worktree" && r.closed.is_none())
            .collect();
        assert_eq!(
            open.len(),
            1,
            "round {round}: one open lane ref: {:?}",
            front.refs
        );
        assert_eq!(open[0].path.as_deref(), Some(lane.to_str().unwrap()));

        // The loser left nothing behind in the repo.
        let after = registered_worktrees(&home, &repo);
        assert_eq!(
            after.len(),
            before + 1,
            "round {round}: one new worktree: {after:?}"
        );
        assert!(after
            .iter()
            .any(|p| p == &lane || p.ends_with(lane.file_name().unwrap())));
        let loser_tag = format!("{}-lane-{loser}", id.to_ascii_lowercase());
        let wt_root = repo.join(".cadence").join("wt");
        let leftovers: Vec<_> = fs::read_dir(&wt_root)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.contains(&loser_tag))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "round {round}: loser {loser} left lane directories: {leftovers:?}"
        );
        let branches = git(
            &home,
            &repo,
            &["branch", "--list", &format!("*{loser_tag}*")],
        );
        assert!(
            branches.is_empty(),
            "round {round}: loser left a branch: {branches}"
        );
    }
}
