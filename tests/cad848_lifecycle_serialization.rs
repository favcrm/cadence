//! Independent CAD-848 acceptance checks for retained-lane start refusal and
//! lifecycle release serialization. All Git repositories, tracker data,
//! review state, and processes are private fixtures under `/tmp`; no live
//! checkout or project state is eligible for mutation.
//!
//! The start case invokes the real `cadence issue start` CLI against an
//! issue with open lane/branch refs and a retained lifecycle record. The
//! finish and review cases pause their real CLIs at actual `git worktree
//! remove` calls, race a retention transition against the held release
//! transaction, then check that release wins without a stale overwrite.

#![cfg(feature = "test-seam")]

use cadence_agent::{
    issue::{self, write, Pm},
    reaper,
    worktree::lifecycle,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const PROJECT: &str = "fixture";
const PR: &str = "41";

fn write_full_proc_mountinfo(proc_root: &Path) {
    fs::create_dir_all(proc_root.join("self/ns")).unwrap();
    fs::create_dir_all(proc_root.join("2/fd")).unwrap();
    fs::create_dir_all(proc_root.join("2/ns")).unwrap();
    fs::write(
        proc_root.join("self/mountinfo"),
        format!(
            "1 23 0:55 / {} rw,nosuid,nodev,noexec,relatime - proc proc rw\n",
            proc_root.canonicalize().unwrap().display()
        ),
    )
    .unwrap();
    fs::write(proc_root.join("self/kernel-release"), "7.0.0\n").unwrap();
    fs::write(
        proc_root.join("2/stat"),
        "2 (kthreadd) S 0 0 0 0 -1 2097152 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
    )
    .unwrap();
    fs::write(
        proc_root.join("2/status"),
        "Name:\tkthreadd\nState:\tS (sleeping)\nUid:\t0 0 0 0\nGid:\t0 0 0 0\nGroups:\t0\nCapEff:\t0000000000000000\n",
    )
    .unwrap();
    ensure_proc_symlink("/", &proc_root.join("2/cwd"));
    ensure_proc_symlink("pid:[42]", &proc_root.join("self/ns/pid"));
    ensure_proc_symlink("pid:[42]", &proc_root.join("2/ns/pid"));
}

fn ensure_proc_symlink(target: &str, path: &Path) {
    if path.is_symlink() {
        return;
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            symlink(target, path).unwrap();
        }
        Err(error) => panic!(
            "cannot inspect synthetic proc symlink {}: {error}",
            path.display()
        ),
    }
}

struct DevelopmentFixture {
    _root: TempDir,
    home: PathBuf,
    state: PathBuf,
    pm: Pm,
    repo: PathBuf,
}

impl DevelopmentFixture {
    fn new() -> Self {
        let root = private_temp("c848-start-");
        let home = root.path().join("home");
        let state = root.path().join("state");
        let repo = root.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        init_repo(&home, &repo);

        let pm = Pm::init(&root.path().join("pm")).expect("isolated PM");
        write::project_add(
            &pm,
            PROJECT,
            "C84",
            &[repo.display().to_string()],
            &[],
            &[],
            None,
        )
        .expect("fixture project");
        let actor = actor();
        write::new_issue(
            &pm,
            &repo,
            Some(PROJECT),
            "retained lane start refusal",
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some("C84-1"),
            Some("Isolated lifecycle serialization fixture."),
            &actor,
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

    fn start(&self) -> Output {
        let actor = actor();
        let mut command = Command::new(BINARY);
        command
            .args([
                "issue",
                "start",
                "C84-1",
                "--repo",
                self.repo.to_str().unwrap(),
                "--name",
                "retained-lane",
                "--owner",
            ])
            .arg(&actor)
            .arg("--by")
            .arg(&actor)
            .current_dir(&self.repo)
            .env("CADENCE_PM_DIR", &self.pm.dir)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        reaper::output(&mut command).expect("run real cadence issue start CLI")
    }
}

#[test]
fn cad848_issue_start_refuses_open_retained_lane_without_mutation() {
    let fixture = DevelopmentFixture::new();
    let created = fixture.start();
    assert!(
        created.status.success(),
        "initial issue start failed:\n{}\n{}",
        String::from_utf8_lossy(&created.stdout),
        String::from_utf8_lossy(&created.stderr)
    );
    let started: Value = serde_json::from_slice(&created.stdout).expect("start JSON");
    let lane = PathBuf::from(started["worktree"].as_str().expect("lane path"));
    let branch = started["branch"].as_str().expect("branch").to_string();
    let sentinel = lane.join(".cad848-retained-lane-sentinel");
    fs::write(&sentinel, b"retain this exact lane\n").unwrap();

    let reason = "CAD-848 acceptance retained for review";
    lifecycle::transition(&fixture.repo, &lane, "retained", Some(reason))
        .expect("record retained state");
    let ledger_path = fixture.repo.join(".cadence/managed-checkouts.json");
    let issue_path = fixture.pm.dir.join(PROJECT).join("C84-1/issue.md");
    let ledger_before = fs::read(&ledger_path).expect("retained lifecycle ledger");
    let issue_before = fs::read(&issue_path).expect("open issue refs");
    let branch_before = git(
        &fixture.home,
        &fixture.repo,
        &["rev-parse", &format!("refs/heads/{branch}")],
    );
    let worktrees_before = git(
        &fixture.home,
        &fixture.repo,
        &["worktree", "list", "--porcelain"],
    );
    let (front, _) = issue::parse::parse_issue(std::str::from_utf8(&issue_before).unwrap())
        .expect("parse fixture issue refs");
    assert!(
        front.refs.iter().any(|reference| {
            reference.kind == "worktree"
                && reference.path.as_deref() == Some(lane.to_str().unwrap())
                && reference.closed != Some(true)
        }),
        "fixture must have an open issue worktree ref"
    );
    assert!(
        front.refs.iter().any(|reference| {
            reference.kind == "branch"
                && reference.path.as_deref() == Some(branch.as_str())
                && reference.closed != Some(true)
        }),
        "fixture must have an open issue branch ref"
    );
    let record_before = lifecycle_record(&ledger_before, &lane);
    assert_eq!(record_before["state"], "retained");
    assert_eq!(record_before["retention_reason"], reason);

    let refused = fixture.start();
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !refused.status.success(),
        "issue start unexpectedly reactivated a retained lane:\n{refusal}"
    );
    assert!(
        refusal.contains("retained") && refusal.contains(reason),
        "refusal must identify retained state and its reason: {refusal}"
    );

    assert!(lane.is_dir(), "retained lane was removed");
    assert_eq!(fs::read(&sentinel).unwrap(), b"retain this exact lane\n");
    assert_eq!(
        fs::read(&ledger_path).unwrap(),
        ledger_before,
        "issue start changed retained lifecycle state or retention_reason"
    );
    assert_eq!(
        fs::read(&issue_path).unwrap(),
        issue_before,
        "issue start changed open tracker refs"
    );
    assert_eq!(
        git(
            &fixture.home,
            &fixture.repo,
            &["rev-parse", &format!("refs/heads/{branch}")]
        ),
        branch_before,
        "issue start moved the retained branch"
    );
    assert_eq!(
        git(
            &fixture.home,
            &fixture.repo,
            &["worktree", "list", "--porcelain"]
        ),
        worktrees_before,
        "issue start changed Git worktree registrations"
    );
}

#[test]
fn cad848_setup_refuses_symlinked_worktree_parent() {
    fn tree_snapshot(root: &Path) -> Value {
        fn collect(root: &Path, dir: &Path, entries: &mut Vec<Value>) {
            use sha2::{Digest, Sha256};

            for entry in fs::read_dir(dir).expect("read private external fixture") {
                let path = entry.expect("read external entry").path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let metadata = fs::symlink_metadata(&path).expect("inspect external entry");
                if metadata.file_type().is_symlink() {
                    entries.push(json!({
                        "path": relative,
                        "kind": "symlink",
                        "target": fs::read_link(&path).unwrap().to_string_lossy(),
                    }));
                } else if metadata.is_dir() {
                    entries.push(json!({"path": relative, "kind": "directory"}));
                    collect(root, &path, entries);
                } else if metadata.is_file() {
                    let bytes = fs::read(&path).expect("read external file");
                    entries.push(json!({
                        "path": relative,
                        "kind": "file",
                        "size": bytes.len(),
                        "sha256": format!("{:x}", Sha256::digest(&bytes)),
                    }));
                } else {
                    entries.push(json!({"path": relative, "kind": "other"}));
                }
            }
        }

        let mut entries = Vec::new();
        collect(root, root, &mut entries);
        entries.sort_by(|left: &Value, right: &Value| {
            left["path"].as_str().cmp(&right["path"].as_str())
        });
        Value::Array(entries)
    }

    let fixture = DevelopmentFixture::new();
    let external = fixture._root.path().join("external");
    fs::create_dir(&external).unwrap();
    let sentinel = external.join("sentinel.txt");
    fs::write(&sentinel, b"fixed external sentinel bytes\n").unwrap();
    let cadence_dir = fixture.repo.join(".cadence");
    fs::create_dir(&cadence_dir).unwrap();
    let worktrees_parent = cadence_dir.join("wt");
    symlink(&external, &worktrees_parent).unwrap();

    let issue_path = fixture.pm.dir.join(PROJECT).join("C84-1/issue.md");
    let ledger_path = cadence_dir.join("managed-checkouts.json");
    let issue_before = fs::read(&issue_path).expect("unstarted issue refs");
    let ledger_before = fs::read(&ledger_path).ok();
    assert!(
        ledger_before.is_none(),
        "fresh fixture unexpectedly has a ledger"
    );
    let worktrees_before = git(
        &fixture.home,
        &fixture.repo,
        &["worktree", "list", "--porcelain"],
    );
    let branch_ref = "refs/heads/cadence/c84-1-retained-lane";
    let branch_before = git(
        &fixture.home,
        &fixture.repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            branch_ref,
        ],
    );
    let external_before = tree_snapshot(&external);

    let output = fixture.start();

    let issue_after = fs::read(&issue_path).ok();
    let ledger_after = fs::read(&ledger_path).ok();
    let worktrees_after = git(
        &fixture.home,
        &fixture.repo,
        &["worktree", "list", "--porcelain"],
    );
    let branch_after = git(
        &fixture.home,
        &fixture.repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            branch_ref,
        ],
    );
    let external_after = tree_snapshot(&external);
    let external_lane = external.join("c84-1-retained-lane");
    let expected_lane = fixture.repo.join(".cadence/wt/c84-1-retained-lane");
    let external_canonical = external_lane.canonicalize().ok();
    let registered_external = worktrees_after.lines().any(|line| {
        line.strip_prefix("worktree ").is_some_and(|path| {
            let path = Path::new(path);
            path == external_lane.as_path()
                || path == expected_lane.as_path()
                || external_canonical
                    .as_deref()
                    .is_some_and(|canonical| path.canonicalize().ok().as_deref() == Some(canonical))
        })
    });
    let ledger_value = ledger_after
        .as_ref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
    let records = ledger_value
        .as_ref()
        .and_then(|value| value["records"].as_array())
        .cloned()
        .unwrap_or_default();
    let record_names_lane = |record: &Value| {
        record["path"].as_str().is_some_and(|path| {
            let path = Path::new(path);
            path == expected_lane.as_path()
                || path == external_lane.as_path()
                || external_canonical
                    .as_deref()
                    .is_some_and(|canonical| path.canonicalize().ok().as_deref() == Some(canonical))
        })
    };
    let matching_records = records
        .iter()
        .filter(|record| record_names_lane(record))
        .count();
    let external_record = records.iter().any(|record| {
        record["path"].as_str().is_some_and(|path| {
            external_canonical.as_deref().is_some_and(|canonical| {
                Path::new(path).canonicalize().ok().as_deref() == Some(canonical)
            })
        })
    });
    let active_record = records.iter().any(|record| {
        record["state"] == "active" && (record["issue"] == "C84-1" || record_names_lane(record))
    });
    let external_lane_exists = external_lane.exists();
    let reached_creation_path =
        external_lane_exists || registered_external || external_record || matching_records > 1;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let issue_after_text = issue_after
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    let ledger_after_text = ledger_after
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    println!(
        "CAD848_SETUP_PARENT_EVIDENCE={}",
        serde_json::to_string(&json!({
            "repo": fixture.repo.display().to_string(),
            "worktrees_parent": worktrees_parent.display().to_string(),
            "external": external.display().to_string(),
            "external_lane": external_lane.display().to_string(),
            "sentinel_before": "fixed external sentinel bytes\n",
            "sentinel_after": fs::read(&sentinel)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
            "external_before": external_before.clone(),
            "external_after": external_after.clone(),
            "external_lane_exists": external_lane_exists,
            "registered_external": registered_external,
            "reached_creation_path": reached_creation_path,
            "worktrees_before": worktrees_before.clone(),
            "worktrees_after": worktrees_after.clone(),
            "branch_before": branch_before.clone(),
            "branch_after": branch_after.clone(),
            "issue_before": String::from_utf8_lossy(&issue_before).into_owned(),
            "issue_after": issue_after_text,
            "ledger_before": ledger_before
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
            "ledger_after": ledger_after_text,
            "lifecycle_records": records,
            "matching_record_count": matching_records,
            "external_record": external_record,
            "active_record": active_record,
            "status_success": output.status.success(),
            "status_code": output.status.code(),
            "stdout": stdout.clone(),
            "stderr": stderr.clone(),
        }))
        .unwrap()
    );

    if output.status.success() {
        assert!(
            reached_creation_path,
            "public issue start succeeded but did not prove that the symlinked destination was used"
        );
    }
    assert!(
        !output.status.success(),
        "issue start unexpectedly accepted a symlinked worktree parent"
    );
    let refusal = format!("{stdout}{stderr}");
    let refusal_lower = refusal.to_ascii_lowercase();
    assert!(
        (refusal.contains(&worktrees_parent.to_string_lossy().to_string())
            || refusal.contains(".cadence/wt")
            || refusal.contains(&external.to_string_lossy().to_string()))
            && ["symlink", "symbolic link", "confin"]
                .iter()
                .any(|word| refusal_lower.contains(*word)),
        "refusal must identify the destination and symlink/confinement issue: {refusal}"
    );
    assert_eq!(external_after, external_before, "external contents changed");
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        b"fixed external sentinel bytes\n",
        "external sentinel changed"
    );
    assert!(
        !external_lane_exists,
        "external linked checkout was created"
    );
    assert!(
        !registered_external,
        "Git registered a checkout at the external destination"
    );
    assert_eq!(
        worktrees_after, worktrees_before,
        "Git worktree registrations changed"
    );
    assert_eq!(branch_after, branch_before, "the issue branch ref changed");
    assert_eq!(
        issue_after.as_deref(),
        Some(issue_before.as_slice()),
        "issue refs changed"
    );
    assert!(
        !external_record,
        "lifecycle ledger names an external checkout"
    );
    assert!(
        !active_record,
        "lifecycle ledger contains an active record for the refused lane"
    );
    assert!(
        matching_records <= 1,
        "lifecycle ledger contains duplicate lane identities"
    );
}

struct ReviewFixture {
    _root: TempDir,
    repo: PathBuf,
    origin: PathBuf,
    state: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    real_git: PathBuf,
    gh_view: PathBuf,
    proc_root: PathBuf,
    tree: PathBuf,
    remove_entered: PathBuf,
    allow_remove: PathBuf,
    remove_probe: PathBuf,
    base_sha: String,
    head_sha: String,
}

impl ReviewFixture {
    fn new() -> Self {
        let root = private_temp("c848-release-race-");
        let repo = root.path().join("repo");
        let origin = root.path().join("origin.git");
        let state = root.path().join("state");
        let home = root.path().join("home");
        let bin = root.path().join("bin");
        let proc_root = root.path().join("proc");
        for dir in [&state, &home, &bin, &proc_root] {
            fs::create_dir_all(dir).unwrap();
        }
        write_full_proc_mountinfo(&proc_root);
        fs::create_dir_all(&repo).unwrap();
        git(&home, &repo, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(
            repo.join("cadence-review.toml"),
            "prepare = []\ngates = [\"true\"]\nfull_suite = \"true\"\n\
             test_globs = [\"tests/**\"]\ntest_command = \"true {test}\"\n\
             stress_pattern = []\n",
        )
        .unwrap();
        fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&home, &repo, &["add", "cadence-review.toml", "base.txt"]);
        git(&home, &repo, &["commit", "--quiet", "-m", "fixture base"]);
        git(&home, &repo, &["checkout", "--quiet", "-b", "pr-head"]);
        fs::write(repo.join("pr-only.txt"), "from PR\n").unwrap();
        git(&home, &repo, &["add", "pr-only.txt"]);
        git(
            &home,
            &repo,
            &["commit", "--quiet", "-m", "fixture PR change"],
        );
        let head_sha = git(&home, &repo, &["rev-parse", "HEAD"]);
        git(&home, &repo, &["checkout", "--quiet", "main"]);
        fs::write(repo.join("base-only.txt"), "from base\n").unwrap();
        git(&home, &repo, &["add", "base-only.txt"]);
        git(
            &home,
            &repo,
            &["commit", "--quiet", "-m", "fixture base advance"],
        );
        let base_sha = git(&home, &repo, &["rev-parse", "HEAD"]);

        fs::create_dir_all(&origin).unwrap();
        git(
            &home,
            &origin,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        git(
            &home,
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(
            &home,
            &repo,
            &["push", "--quiet", "origin", "main:refs/heads/main"],
        );
        git(
            &home,
            &repo,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("{head_sha}:refs/pull/{PR}/head"),
            ],
        );

        let gh_view = root.path().join("pr-view.json");
        fs::write(
            &gh_view,
            serde_json::to_vec(&json!({
                "number": 41,
                "title": "CAD-848 release race fixture",
                "url": "https://example.invalid/pr/41",
                "headRefName": "pr-head",
                "headRefOid": head_sha,
                "baseRefName": "main",
                "files": [{"path": "pr-only.txt"}],
                "state": "OPEN"
            }))
            .unwrap(),
        )
        .unwrap();
        write_executable(
            &bin.join("gh"),
            r##"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view") /bin/cat "$CADENCE_CAD848_GH_VIEW" ;;
  "pr list") printf '[]\n' ;;
  *) printf 'unexpected gh command: %s\n' "$*" >&2; exit 2 ;;
esac
"##,
        );
        let real_git = find_on_path("git");
        write_executable(&bin.join("git"), GIT_REMOVAL_OBSERVER);

        let tree = repo.join(".cadence/wt/review-41");
        let remove_entered = root.path().join("remove-entered");
        let allow_remove = root.path().join("allow-remove");
        let remove_probe = root.path().join("remove-probe.json");
        Self {
            _root: root,
            repo,
            origin,
            state,
            home,
            bin,
            real_git,
            gh_view,
            proc_root,
            tree,
            remove_entered,
            allow_remove,
            remove_probe,
            base_sha,
            head_sha,
        }
    }

    fn start_review(&self) -> PausedRemoval {
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths).unwrap();
        let stdout_path = self._root.path().join("review.stdout");
        let stderr_path = self._root.path().join("review.stderr");
        let stdout = fs::File::create(&stdout_path).unwrap();
        let stderr = fs::File::create(&stderr_path).unwrap();
        let mut command = Command::new(BINARY);
        command
            .args([
                "--state-dir",
                self.state.to_str().unwrap(),
                "review",
                PR,
                "--repo",
                "fixture/repo",
                "--no-full",
                "--json",
            ])
            .current_dir(&self.repo)
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_AUTHOR_NAME", "CAD-848 fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@invalid")
            .env("GIT_COMMITTER_NAME", "CAD-848 fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@invalid")
            .env("CADENCE_CAD848_REAL_GIT", &self.real_git)
            .env("CADENCE_CAD848_TARGET_TREE", &self.tree)
            .env("CADENCE_CAD848_REMOVE_ENTERED", &self.remove_entered)
            .env("CADENCE_CAD848_ALLOW_REMOVE", &self.allow_remove)
            .env("CADENCE_CAD848_REMOVE_PROBE", &self.remove_probe)
            .env("CADENCE_CAD848_GH_VIEW", &self.gh_view)
            .env("CADENCE_TEST_PROC_ROOT", &self.proc_root)
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_ORG")
            .env_remove("CADENCE_HOME")
            .env_remove("CADENCE_PROFILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let child = reaper::spawn(&mut command).expect("start real cadence review CLI");
        PausedRemoval {
            child: Some(child),
            allow_remove: self.allow_remove.clone(),
            stdout_path,
            stderr_path,
        }
    }

    fn record_bytes(&self) -> Vec<u8> {
        fs::read(self.repo.join(".cadence/managed-checkouts.json"))
            .expect("review lifecycle ledger")
    }

    fn record(&self) -> Value {
        lifecycle_record(&self.record_bytes(), &self.tree)
    }
}

struct PausedRemoval {
    child: Option<Child>,
    allow_remove: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl PausedRemoval {
    fn allow_removal(&self) {
        fs::write(&self.allow_remove, b"continue\n").expect("release fixture Git gate");
    }

    fn wait_for_removal_gate(&mut self, marker: &Path) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if marker.is_file() {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!(
                    "fixture CLI exited before real removal was gated ({status}):\n{}\n{}",
                    fs::read_to_string(&self.stdout_path).unwrap_or_default(),
                    fs::read_to_string(&self.stderr_path).unwrap_or_default()
                );
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for ReviewTree removal"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(mut self) -> Output {
        self.allow_removal();
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("fixture CLI exceeded its 60-second bound");
            }
            thread::sleep(Duration::from_millis(20));
        };
        Output {
            status,
            stdout: fs::read(&self.stdout_path).unwrap_or_default(),
            stderr: fs::read(&self.stderr_path).unwrap_or_default(),
        }
    }
}

impl Drop for PausedRemoval {
    fn drop(&mut self) {
        let _ = fs::write(&self.allow_remove, b"continue after test unwind\n");
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(35);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }
}

#[test]
fn cad848_review_release_lock_defeats_concurrent_retain() {
    let fixture = ReviewFixture::new();
    let mut review = fixture.start_review();
    review.wait_for_removal_gate(&fixture.remove_entered);

    let before = fixture.record();
    assert_eq!(
        before["state"], "releasing",
        "release lock must precede deletion"
    );
    assert_eq!(before["release_reason"], "review completed");
    assert!(
        fixture.tree.is_dir(),
        "checkout must exist before gated removal"
    );
    assert!(registered(&fixture.home, &fixture.repo, &fixture.tree));

    let repo = fixture.repo.clone();
    let tree = fixture.tree.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let retain_thread = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result =
            lifecycle::transition(&repo, &tree, "retained", Some("racing retain must not win"))
                .map_err(|error| error.to_string());
        let _ = done_tx.send(result);
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("concurrent retain attempt started");

    // The real ReviewTree removal is paused at Git with `releasing` already
    // durable. A retain may reject immediately or wait for the held ledger
    // lock, but it must never report success during deletion.
    let early_result = match done_rx.recv_timeout(Duration::from_millis(200)) {
        Ok(Ok(())) => panic!("retain reported success while ReviewTree release was in progress"),
        Ok(Err(error)) => Some(Err(error)),
        Err(mpsc::RecvTimeoutError::Timeout) => None,
        Err(error) => panic!("retain worker disconnected: {error}"),
    };
    assert_eq!(fixture.record()["state"], "releasing");
    assert!(
        fixture.tree.is_dir(),
        "checkout vanished before Git removal was allowed"
    );

    review.allow_removal();
    let output = review.finish();
    assert!(
        output.status.success(),
        "cadence review failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let retain_result = match early_result {
        Some(result) => result,
        None => done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("retain attempt completes after release"),
    };
    let retain_refusal = retain_result
        .expect_err("concurrent retain must not overwrite completed release")
        .to_ascii_lowercase();
    assert!(
        retain_refusal.contains("releas"),
        "retain refusal must identify the release boundary: {retain_refusal}"
    );
    retain_thread.join().expect("retain thread");

    assert!(
        !fixture.tree.exists(),
        "ReviewTree release left its checkout behind"
    );
    assert!(!registered(&fixture.home, &fixture.repo, &fixture.tree));
    let after = fixture.record();
    assert_eq!(after["state"], "released");
    assert_eq!(after["release_reason"], "review completed");
    assert!(after["retention_reason"].is_null());
    let probe: Value =
        serde_json::from_slice(&fs::read(&fixture.remove_probe).expect("actual git removal probe"))
            .unwrap();
    assert_eq!(probe["state_before_remove"], "releasing");
    assert!(
        !probe["force"].as_bool().unwrap_or(true),
        "release must not force-remove the checkout"
    );
    assert!(probe["remove_succeeded"].as_bool().unwrap_or(false));
    assert_eq!(
        git(
            &fixture.home,
            &fixture.repo,
            &["rev-parse", "refs/heads/main"]
        ),
        fixture.base_sha
    );
    assert_eq!(
        git(
            &fixture.home,
            &fixture.origin,
            &["rev-parse", &format!("refs/pull/{PR}/head")]
        ),
        fixture.head_sha
    );
}

struct FinishFixture {
    development: DevelopmentFixture,
    lane: PathBuf,
    branch: String,
    branch_tip: String,
    bin: PathBuf,
    real_git: PathBuf,
    proc_root: PathBuf,
    remove_entered: PathBuf,
    allow_remove: PathBuf,
    remove_probe: PathBuf,
}

impl FinishFixture {
    fn new() -> Self {
        let development = DevelopmentFixture::new();
        let bin = development._root.path().join("bin");
        let proc_root = development._root.path().join("proc");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&proc_root).unwrap();
        write_full_proc_mountinfo(&proc_root);
        let real_git = find_on_path("git");
        write_executable(&bin.join("git"), GIT_REMOVAL_OBSERVER);

        let started = development.start();
        assert!(
            started.status.success(),
            "initial issue start failed:\n{}\n{}",
            String::from_utf8_lossy(&started.stdout),
            String::from_utf8_lossy(&started.stderr)
        );
        let started: Value = serde_json::from_slice(&started.stdout).expect("start JSON");
        let lane = PathBuf::from(started["worktree"].as_str().expect("lane path"));
        let branch = started["branch"].as_str().expect("branch").to_string();
        fs::write(lane.join("work.txt"), "merged finish fixture\n").unwrap();
        git(&development.home, &lane, &["add", "work.txt"]);
        git(
            &development.home,
            &lane,
            &["commit", "--quiet", "-m", "fixture merged work"],
        );
        git(
            &development.home,
            &development.repo,
            &["merge", "--quiet", "--ff-only", &branch],
        );
        git(
            &development.home,
            &development.repo,
            &["push", "--quiet", "origin", "main"],
        );
        git(
            &development.home,
            &development.repo,
            &["fetch", "--quiet", "origin"],
        );
        age_tracked_files(&lane);
        let branch_tip = git(
            &development.home,
            &development.repo,
            &["rev-parse", &format!("refs/heads/{branch}")],
        );

        let root = development._root.path().to_path_buf();
        let remove_entered = root.join("finish-remove-entered");
        let allow_remove = root.join("finish-allow-remove");
        let remove_probe = root.join("finish-remove-probe.json");
        Self {
            development,
            lane,
            branch,
            branch_tip,
            bin,
            real_git,
            proc_root,
            remove_entered,
            allow_remove,
            remove_probe,
        }
    }

    fn start_finish(&self) -> PausedRemoval {
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths).unwrap();
        let stdout_path = self.development._root.path().join("finish.stdout");
        let stderr_path = self.development._root.path().join("finish.stderr");
        let stdout = fs::File::create(&stdout_path).unwrap();
        let stderr = fs::File::create(&stderr_path).unwrap();
        let mut command = Command::new(BINARY);
        command
            .args([
                "issue",
                "finish",
                "C84-1",
                "--worktree",
                self.lane.to_str().unwrap(),
            ])
            .current_dir(&self.development.repo)
            .env("PATH", path)
            .env("CADENCE_PM_DIR", &self.development.pm.dir)
            .env("HOME", &self.development.home)
            .env("XDG_STATE_HOME", &self.development.state)
            .env("XDG_CONFIG_HOME", self.development.home.join("config"))
            .env("XDG_DATA_HOME", self.development.home.join("data"))
            .env("XDG_CACHE_HOME", self.development.home.join("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.development.home.join("gitconfig"))
            .env("CADENCE_TEST_PROC_ROOT", &self.proc_root)
            .env("CADENCE_CAD848_REAL_GIT", &self.real_git)
            .env("CADENCE_CAD848_TARGET_TREE", &self.lane)
            .env("CADENCE_CAD848_REMOVE_ENTERED", &self.remove_entered)
            .env("CADENCE_CAD848_ALLOW_REMOVE", &self.allow_remove)
            .env("CADENCE_CAD848_REMOVE_PROBE", &self.remove_probe)
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let child = reaper::spawn(&mut command).expect("start real cadence issue finish CLI");
        PausedRemoval {
            child: Some(child),
            allow_remove: self.allow_remove.clone(),
            stdout_path,
            stderr_path,
        }
    }

    fn record(&self) -> Value {
        let bytes = fs::read(
            self.development
                .repo
                .join(".cadence/managed-checkouts.json"),
        )
        .expect("development lifecycle ledger");
        lifecycle_record(&bytes, &self.lane)
    }
}

#[test]
fn cad848_issue_finish_release_lock_defeats_concurrent_retain() {
    let fixture = FinishFixture::new();
    let mut finish = fixture.start_finish();
    finish.wait_for_removal_gate(&fixture.remove_entered);

    let before = fixture.record();
    assert_eq!(before["state"], "releasing");
    assert_eq!(before["purpose"], "development");
    assert_eq!(before["issue"], "C84-1");
    assert_eq!(before["branch"], fixture.branch);
    assert!(
        fixture.lane.is_dir(),
        "finish removed lane before real Git gate"
    );
    assert!(registered(
        &fixture.development.home,
        &fixture.development.repo,
        &fixture.lane
    ));
    assert_eq!(
        git(
            &fixture.development.home,
            &fixture.development.repo,
            &["rev-parse", &format!("refs/heads/{}", fixture.branch)]
        ),
        fixture.branch_tip
    );

    let repo = fixture.development.repo.clone();
    let lane = fixture.lane.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let retain_thread = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = lifecycle::transition(
            &repo,
            &lane,
            "retained",
            Some("racing development retain must not win"),
        )
        .map_err(|error| error.to_string());
        let _ = done_tx.send(result);
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("concurrent development retain attempt started");
    let early_result = match done_rx.recv_timeout(Duration::from_millis(200)) {
        Ok(Ok(())) => panic!("retain reported success while issue finish was in progress"),
        Ok(Err(error)) => Some(Err(error)),
        Err(mpsc::RecvTimeoutError::Timeout) => None,
        Err(error) => panic!("development retain worker disconnected: {error}"),
    };
    assert_eq!(fixture.record()["state"], "releasing");
    assert!(
        fixture.lane.is_dir(),
        "lane vanished before removal was allowed"
    );

    finish.allow_removal();
    let output = finish.finish();
    assert!(
        output.status.success(),
        "cadence issue finish failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let retain_result = match early_result {
        Some(result) => result,
        None => done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("development retain completes after release"),
    };
    let retain_refusal = retain_result
        .expect_err("concurrent retain must not overwrite finish release")
        .to_ascii_lowercase();
    assert!(
        retain_refusal.contains("releas"),
        "development retain refusal must identify the release boundary: {retain_refusal}"
    );
    retain_thread.join().expect("development retain thread");

    assert!(
        !fixture.lane.exists(),
        "finish left the released lane behind"
    );
    assert!(!registered(
        &fixture.development.home,
        &fixture.development.repo,
        &fixture.lane
    ));
    assert!(
        git(
            &fixture.development.home,
            &fixture.development.repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/heads/{}", fixture.branch),
            ]
        )
        .is_empty(),
        "merged development branch survived finish"
    );
    let after = fixture.record();
    assert_eq!(after["state"], "released");
    assert!(after["release_reason"]
        .as_str()
        .unwrap()
        .contains("checkout released"));
    assert!(after["retention_reason"].is_null());
    let issue_path = fixture
        .development
        .pm
        .dir
        .join(PROJECT)
        .join("C84-1/issue.md");
    let issue_text = fs::read_to_string(issue_path).unwrap();
    let (front, _) = issue::parse::parse_issue(&issue_text).unwrap();
    assert!(front.refs.iter().any(|reference| {
        reference.kind == "worktree"
            && reference.path.as_deref() == Some(fixture.lane.to_str().unwrap())
            && reference.closed == Some(true)
    }));
    assert!(front.refs.iter().any(|reference| {
        reference.kind == "branch"
            && reference.path.as_deref() == Some(fixture.branch.as_str())
            && reference.closed == Some(true)
    }));
    let probe: Value = serde_json::from_slice(
        &fs::read(&fixture.remove_probe).expect("actual finish git removal probe"),
    )
    .unwrap();
    assert_eq!(probe["state_before_remove"], "releasing");
    assert!(!probe["force"].as_bool().unwrap_or(true));
    assert!(probe["remove_succeeded"].as_bool().unwrap_or(false));
}

#[test]
fn cad848_target_reclaim_guard_serializes_retain_and_adopt() {
    let fixture = DevelopmentFixture::new();
    let started = fixture.start();
    assert!(started.status.success(), "initial issue start failed");
    let started: Value = serde_json::from_slice(&started.stdout).unwrap();
    let lane = PathBuf::from(started["worktree"].as_str().unwrap());
    let branch = started["branch"].as_str().unwrap().to_string();
    let target = lane.join("target");
    fs::create_dir_all(&target).unwrap();
    let sentinel = target.join(".cad848-reclaim-guard-sentinel");
    fs::write(&sentinel, b"reclaim guard owns no lifecycle mutation\n").unwrap();
    let ledger_path = fixture.repo.join(".cadence/managed-checkouts.json");
    let issue_path = fixture.pm.dir.join(PROJECT).join("C84-1/issue.md");
    let ledger_before = fs::read(&ledger_path).unwrap();
    let issue_before = fs::read(&issue_path).unwrap();
    let branch_before = git(
        &fixture.home,
        &fixture.repo,
        &["rev-parse", &format!("refs/heads/{branch}")],
    );
    let trees_before = git(
        &fixture.home,
        &fixture.repo,
        &["worktree", "list", "--porcelain"],
    );

    let guard = lifecycle::begin_target_reclaim(&fixture.repo, &lane, "C84-1", &branch)
        .expect("acquire read-only target-reclaim guard");
    assert_eq!(fs::read(&ledger_path).unwrap(), ledger_before);
    let repo = fixture.repo.clone();
    let lane_for_retain = lane.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let retain_thread = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = lifecycle::transition(
            &repo,
            &lane_for_retain,
            "retained",
            Some("retain waits for target reclamation guard"),
        )
        .map_err(|error| error.to_string());
        let _ = done_tx.send(result);
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("concurrent retain attempt started");
    assert!(
        matches!(
            done_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "retain completed while target reclamation owned the lifecycle guard"
    );
    assert_eq!(fs::read(&ledger_path).unwrap(), ledger_before);
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        b"reclaim guard owns no lifecycle mutation\n"
    );
    assert!(
        lane.join("tracked.txt").is_file(),
        "retain race deleted source"
    );
    assert_eq!(fs::read(&issue_path).unwrap(), issue_before);
    assert_eq!(
        git(
            &fixture.home,
            &fixture.repo,
            &["rev-parse", &format!("refs/heads/{branch}")],
        ),
        branch_before
    );
    assert_eq!(
        git(
            &fixture.home,
            &fixture.repo,
            &["worktree", "list", "--porcelain"],
        ),
        trees_before
    );
    drop(guard);
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("retain completes after guard release")
        .expect("retain proceeds only after target-reclaim guard release");
    retain_thread.join().unwrap();
    let retained_ledger = fs::read(&ledger_path).unwrap();
    let retained_record = lifecycle_record(&retained_ledger, &lane);
    assert_eq!(retained_record["state"], "retained");
    assert_eq!(
        retained_record["retention_reason"],
        "retain waits for target reclamation guard"
    );
    assert!(
        lifecycle::begin_target_reclaim(&fixture.repo, &lane, "C84-1", &branch).is_err(),
        "a retain completed before acquisition must prevent target reclamation"
    );
    assert_eq!(fs::read(&ledger_path).unwrap(), retained_ledger);
    assert_eq!(fs::read(&issue_path).unwrap(), issue_before);
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        b"reclaim guard owns no lifecycle mutation\n"
    );
    assert!(
        lane.join("tracked.txt").is_file(),
        "retained target refusal deleted source"
    );
    assert_eq!(
        git(
            &fixture.home,
            &fixture.repo,
            &["rev-parse", &format!("refs/heads/{branch}")],
        ),
        branch_before
    );
    assert_eq!(
        git(
            &fixture.home,
            &fixture.repo,
            &["worktree", "list", "--porcelain"],
        ),
        trees_before
    );

    let adoption_fixture = DevelopmentFixture::new();
    let started = adoption_fixture.start();
    assert!(started.status.success(), "legacy lane setup failed");
    let started: Value = serde_json::from_slice(&started.stdout).unwrap();
    let legacy_lane = PathBuf::from(started["worktree"].as_str().unwrap());
    let legacy_branch = started["branch"].as_str().unwrap().to_string();
    let legacy_tip = git(&adoption_fixture.home, &legacy_lane, &["rev-parse", "HEAD"]);
    let legacy_target = legacy_lane.join("target");
    fs::create_dir_all(&legacy_target).unwrap();
    let legacy_sentinel = legacy_target.join(".cad848-legacy-reclaim-sentinel");
    fs::write(&legacy_sentinel, b"legacy cache remains untouched\n").unwrap();
    let adoption_ledger_path = adoption_fixture
        .repo
        .join(".cadence/managed-checkouts.json");
    let issue_before =
        fs::read(adoption_fixture.pm.dir.join(PROJECT).join("C84-1/issue.md")).unwrap();
    let mut legacy_ledger: Value =
        serde_json::from_slice(&fs::read(&adoption_ledger_path).unwrap()).unwrap();
    legacy_ledger["records"]
        .as_array_mut()
        .unwrap()
        .retain(|record| record["path"].as_str() != Some(legacy_lane.to_str().unwrap()));
    fs::write(
        &adoption_ledger_path,
        serde_json::to_vec_pretty(&legacy_ledger).unwrap(),
    )
    .unwrap();
    let legacy_ledger_before = fs::read(&adoption_ledger_path).unwrap();
    let owner = actor();
    let repo = adoption_fixture.repo.canonicalize().unwrap();
    let adoption = lifecycle::new_record(lifecycle::CheckoutSpec {
        repo: &repo,
        purpose: "development",
        tool: "cadence issue start",
        owner: &owner,
        path: &legacy_lane,
        branch: Some(&legacy_branch),
        pinned_sha: &legacy_tip,
        issue: Some("C84-1"),
    });
    let legacy_guard = lifecycle::begin_target_reclaim(
        &adoption_fixture.repo,
        &legacy_lane,
        "C84-1",
        &legacy_branch,
    )
    .expect("legacy issue-ref checkout guard fences explicit adoption");
    assert_eq!(
        fs::read(&adoption_ledger_path).unwrap(),
        legacy_ledger_before
    );
    let repo = adoption_fixture.repo.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let adoption_thread = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = lifecycle::adopt(&repo, adoption).map_err(|error| error.to_string());
        let _ = done_tx.send(result);
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("adoption attempt started");
    assert!(
        matches!(
            done_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "adoption completed while target reclamation owned the lifecycle guard"
    );
    assert_eq!(
        fs::read(&adoption_ledger_path).unwrap(),
        legacy_ledger_before
    );
    assert_eq!(
        fs::read(&legacy_sentinel).unwrap(),
        b"legacy cache remains untouched\n"
    );
    assert!(
        legacy_lane.join("tracked.txt").is_file(),
        "adoption race deleted source"
    );
    assert_eq!(
        fs::read(adoption_fixture.pm.dir.join(PROJECT).join("C84-1/issue.md")).unwrap(),
        issue_before
    );
    drop(legacy_guard);
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("adoption completes after target-reclaim guard release")
        .expect("explicit adoption proceeds after guard release");
    adoption_thread.join().unwrap();
    let adopted = lifecycle_record(&fs::read(&adoption_ledger_path).unwrap(), &legacy_lane);
    assert_eq!(adopted["state"], "active");
    assert_eq!(adopted["issue"], "C84-1");
    assert_eq!(adopted["branch"], legacy_branch);
    assert_eq!(
        fs::read(&legacy_sentinel).unwrap(),
        b"legacy cache remains untouched\n"
    );
    assert!(
        legacy_lane.join("tracked.txt").is_file(),
        "adoption deleted source"
    );
    assert_eq!(
        git(
            &adoption_fixture.home,
            &adoption_fixture.repo,
            &["rev-parse", &legacy_branch],
        ),
        legacy_tip
    );
    assert!(registered(
        &adoption_fixture.home,
        &adoption_fixture.repo,
        &legacy_lane
    ));
    assert_eq!(
        fs::read(adoption_fixture.pm.dir.join(PROJECT).join("C84-1/issue.md")).unwrap(),
        issue_before
    );
}

fn lifecycle_record(bytes: &[u8], path: &Path) -> Value {
    let ledger: Value = serde_json::from_slice(bytes).expect("decode lifecycle ledger");
    ledger["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["path"].as_str() == Some(path.to_str().unwrap()))
        .cloned()
        .expect("fixture checkout lifecycle record")
}

fn actor() -> String {
    std::env::var("CADENCE_ALIAS").unwrap_or_else(|_| "cad848-fixture".to_string())
}

fn private_temp(prefix: &str) -> TempDir {
    let root = Builder::new().prefix(prefix).tempdir_in("/tmp").unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "user.name=CAD-848 isolated fixture",
            "-c",
            "user.email=fixture@invalid",
        ])
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    let output = reaper::output(&mut command).expect("run fixture Git");
    assert!(
        output.status.success(),
        "git {} in {} failed:\n{}{}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo(home: &Path, repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git(home, repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join(".gitignore"), "target/\n.cadence/\n").unwrap();
    fs::write(repo.join("tracked.txt"), "fixture base\n").unwrap();
    git(home, repo, &["add", ".gitignore", "tracked.txt"]);
    git(home, repo, &["commit", "--quiet", "-m", "fixture base"]);
    let origin = repo.with_file_name("origin.git");
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

fn age_tracked_files(lane: &Path) {
    let then = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(2 * 60 * 60);
    for name in [".gitignore", "tracked.txt", "work.txt"] {
        let mut command = Command::new("touch");
        command
            .args(["-h", "-d", &format!("@{then}")])
            .arg(lane.join(name));
        let output = reaper::output(&mut command).expect("age fixture source files");
        assert!(
            output.status.success(),
            "touch {} failed: {}",
            name,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn registered(home: &Path, repo: &Path, path: &Path) -> bool {
    git(home, repo, &["worktree", "list", "--porcelain"])
        .lines()
        .any(|line| line == format!("worktree {}", path.display()))
}

fn find_on_path(program: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
        .expect("real Git on PATH")
        .canonicalize()
        .unwrap()
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

const GIT_REMOVAL_OBSERVER: &str = r##"#!/usr/bin/env python3
import json, os, subprocess, sys, time

args = sys.argv[1:]
real = os.environ["CADENCE_CAD848_REAL_GIT"]
tree = os.path.realpath(os.environ["CADENCE_CAD848_TARGET_TREE"])
entered = os.environ["CADENCE_CAD848_REMOVE_ENTERED"]
allow = os.environ["CADENCE_CAD848_ALLOW_REMOVE"]
probe = os.environ["CADENCE_CAD848_REMOVE_PROBE"]

remove = "worktree" in args and "remove" in args and any(
    not arg.startswith("-") and os.path.realpath(arg) == tree for arg in args
)
if remove:
    # The ledger is at <repo>/.cadence/managed-checkouts.json; derive its
    # parent from the known layout rather than trust a caller-supplied path.
    repo = os.path.dirname(os.path.dirname(os.path.dirname(tree)))
    record_path = os.path.join(repo, ".cadence", "managed-checkouts.json")
    records = json.load(open(record_path, encoding="utf-8"))["records"]
    record = next(row for row in records if os.path.realpath(row["path"]) == tree)
    force = "--force" in args or "-f" in args
    event = {"state_before_remove": record["state"], "force": force}
    with open(probe, "w", encoding="utf-8") as stream:
        json.dump(event, stream)
    with open(entered, "w", encoding="utf-8") as stream:
        stream.write("paused before real git worktree remove\n")
    deadline = time.monotonic() + 30
    while not os.path.exists(allow):
        if time.monotonic() >= deadline:
            print("timed out waiting for isolated acceptance test", file=sys.stderr)
            sys.exit(95)
        time.sleep(0.01)
    result = subprocess.run([real, *args], env=os.environ.copy())
    event["remove_succeeded"] = result.returncode == 0 and not os.path.exists(tree)
    with open(probe, "w", encoding="utf-8") as stream:
        json.dump(event, stream)
    sys.exit(result.returncode)

os.execvpe(real, [real, *args], os.environ.copy())
"##;
