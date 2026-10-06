//! CAD-848 reviewer-owned acceptance check for clean merge-result review release.
//!
//! Drive the real `cadence review` CLI against an isolated bare origin. The
//! observing `git` shim forwards to real Git; nonblocking flock probes verify
//! the lifecycle lock spans merge abort, pinned-head restoration, and removal.
//! At removal it also proves the pinned head, clean tree, and durable report
//! receipts are already in place. A separate run injects an external checkout
//! change and proves the tree remains registered with that change intact and
//! no destructive cleanup is attempted. The `test-seam` feature points the
//! real live-use guard at an empty private proc root so unrelated host processes
//! cannot influence the
//! lifecycle checks; process classification is covered by its dedicated test.

#![cfg(feature = "test-seam")]

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const PR: &str = "41";
static REVIEW_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    origin: PathBuf,
    state: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    real_git: PathBuf,
    events: PathBuf,
    base_sha: String,
    head_sha: String,
    review_tree: PathBuf,
    changed_marker: PathBuf,
    proc_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("c848review-")
            .tempdir_in("/tmp")
            .expect("isolated review fixture");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))
            .expect("restrict review fixture root to its owner");
        let proc_root = root.path().join("proc");
        fs::create_dir(&proc_root).unwrap();
        let repo = root.path().join("repo");
        let origin = root.path().join("origin.git");
        let state = root.path().join("state");
        let home = root.path().join("home");
        let bin = root.path().join("bin");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();

        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(
            repo.join("cadence-review.toml"),
            "prepare = []\ngates = [\"true\"]\nfull_suite = \"true\"\n\
             test_globs = [\"tests/**\"]\ntest_command = \"true {test}\"\n\
             stress_pattern = []\n",
        )
        .unwrap();
        fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "cadence-review.toml", "base.txt"]);
        git(&repo, &["commit", "--quiet", "-m", "fixture base"]);

        git(&repo, &["checkout", "--quiet", "-b", "pr-head"]);
        fs::write(repo.join("pr-only.txt"), "from PR\n").unwrap();
        git(&repo, &["add", "pr-only.txt"]);
        git(&repo, &["commit", "--quiet", "-m", "fixture PR change"]);
        let head_sha = git(&repo, &["rev-parse", "HEAD"]);

        git(&repo, &["checkout", "--quiet", "main"]);
        fs::write(repo.join("base-only.txt"), "from base\n").unwrap();
        git(&repo, &["add", "base-only.txt"]);
        git(&repo, &["commit", "--quiet", "-m", "fixture base advance"]);
        let base_sha = git(&repo, &["rev-parse", "HEAD"]);

        fs::create_dir_all(&origin).unwrap();
        git(
            &origin,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(
            &repo,
            &["push", "--quiet", "origin", "main:refs/heads/main"],
        );
        git(
            &repo,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("{head_sha}:refs/pull/{PR}/head"),
            ],
        );

        let real_git = find_on_path("git");
        let events = root.path().join("git-events.jsonl");
        let review_tree = repo.join(".cadence/wt/review-41");
        let changed_marker = review_tree.join(".cad848-external-change");

        let gh_view = root.path().join("pr-view.json");
        fs::write(
            &gh_view,
            serde_json::to_vec(&json!({
                "number": 41,
                "title": "CAD-848 lifecycle fixture",
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
  "pr list")
    if [ -n "${CADENCE_CAD848_TAMPER_SENTINEL:-}" ]; then
      printf 'external mutation after gates\n' > "$CADENCE_CAD848_TAMPER_SENTINEL"
    fi
    printf '[]\n'
    ;;
  *) printf 'unexpected gh command: %s\n' "$*" >&2; exit 2 ;;
esac
"##,
        );
        write_executable(&bin.join("git"), GIT_OBSERVER);

        Self {
            _root: root,
            repo,
            origin,
            state,
            home,
            bin,
            real_git,
            events,
            base_sha,
            head_sha,
            review_tree,
            changed_marker,
            proc_root,
        }
    }

    fn review(&self, externally_change_tree: bool, run_id: &str) -> Output {
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths).unwrap();
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
            .env("CADENCE_CAD848_REAL_GIT", &self.real_git)
            .env("CADENCE_CAD848_ROOT", &self.repo)
            .env("CADENCE_CAD848_REVIEW_TREE", &self.review_tree)
            .env("CADENCE_CAD848_EXPECTED_HEAD", &self.head_sha)
            .env("CADENCE_CAD848_PROBE", self.probe_path(run_id))
            .env("CADENCE_CAD848_EVENTS", &self.events)
            .env("CADENCE_CAD848_RUN_ID", run_id)
            .env("CADENCE_TEST_PROC_ROOT", &self.proc_root)
            .env(
                "CADENCE_CAD848_GH_VIEW",
                self._root.path().join("pr-view.json"),
            )
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_ORG")
            .env_remove("CADENCE_HOME")
            .env_remove("CADENCE_PROFILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        if externally_change_tree {
            command.env("CADENCE_CAD848_TAMPER_SENTINEL", &self.changed_marker);
        } else {
            command.env_remove("CADENCE_CAD848_TAMPER_SENTINEL");
        }
        cadence_agent::reaper::output(&mut command).expect("run real cadence review CLI")
    }

    fn probe_path(&self, run_id: &str) -> PathBuf {
        self._root
            .path()
            .join(format!("release-probe-{run_id}.json"))
    }

    fn record(&self) -> Value {
        let ledger: Value = serde_json::from_slice(
            &fs::read(self.repo.join(".cadence/managed-checkouts.json"))
                .expect("managed checkout ledger"),
        )
        .unwrap();
        ledger["records"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|row| row["path"].as_str() == Some(self.review_tree.to_str().unwrap()))
            .cloned()
            .expect("review checkout lifecycle record")
    }
}

#[test]
fn cad848_clean_merge_result_is_restored_before_receipted_release() {
    let _lock = REVIEW_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();

    let first = fixture.review(false, "first");
    let first_report = assert_clean_release(&fixture, &first, "first");
    assert_eq!(
        first_report["worktree"],
        fixture.review_tree.to_string_lossy().to_string()
    );
    let first_artifacts = fixture.record()["release_artifacts"].clone();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // An explicit second review of the same PR must create and release a new
    // generation after the prior checkout is absent and unregistered.
    let second = fixture.review(false, "second");
    let second_report = assert_clean_release(&fixture, &second, "second");
    assert_eq!(second_report["worktree"], first_report["worktree"]);
    assert_eq!(second_report["head"], first_report["head"]);
    assert_ne!(second_report["report_md"], first_report["report_md"]);
    for artifact in first_artifacts.as_array().unwrap() {
        assert!(Path::new(artifact.as_str().unwrap()).is_file());
    }
    assert_release_guard_events(&fixture, "first");
    assert_release_guard_events(&fixture, "second");
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "refs/heads/main"]),
        fixture.base_sha
    );
    assert_eq!(
        git(
            &fixture.origin,
            &["rev-parse", &format!("refs/pull/{PR}/head")]
        ),
        fixture.head_sha
    );
}

fn assert_clean_release(fixture: &Fixture, output: &Output, run_id: &str) -> Value {
    assert_review_succeeded(output);
    let report: Value = serde_json::from_slice(&output.stdout).expect("review JSON report");
    assert_eq!(report["pr"], PR.parse::<i64>().unwrap());
    assert_eq!(report["gated_tree"], "merge-result");
    assert_eq!(report["merge"]["result"], "clean");
    assert_eq!(report["head"], fixture.head_sha);
    assert_eq!(report["base"]["sha"], fixture.base_sha);

    let probe: Value = serde_json::from_slice(
        &fs::read(fixture.probe_path(run_id)).expect("release-time Git observer probe"),
    )
    .unwrap();
    assert_eq!(probe["run_id"], run_id);
    assert_eq!(probe["head"], fixture.head_sha);
    assert_eq!(probe["clean"], true);
    assert_eq!(probe["detached"], true);
    assert_eq!(probe["record_state"], "releasing");
    assert_eq!(probe["artifacts_recorded"], true);
    assert_eq!(probe["artifacts_exist"], true);
    assert_eq!(probe["artifacts_outside_tree"], true);
    assert_eq!(probe["report_matches"], true);
    assert_eq!(probe["removed"], true);

    assert!(
        !fixture.review_tree.exists(),
        "released checkout directory remains"
    );
    assert!(!registered(&fixture.repo, &fixture.review_tree));
    let record = fixture.record();
    assert_eq!(record["state"], "released");
    assert_eq!(record["owner"], caller_identity());
    assert_eq!(record["pinned_sha"], fixture.head_sha);
    assert_eq!(record["release_artifacts"].as_array().unwrap().len(), 2);
    for artifact in record["release_artifacts"].as_array().unwrap() {
        assert!(Path::new(artifact.as_str().unwrap()).is_file());
    }
    assert!(Path::new(report["report_md"].as_str().unwrap()).is_file());
    report
}

fn assert_release_guard_events(fixture: &Fixture, run_id: &str) {
    let mut phases = Vec::new();
    let events = fs::read_to_string(&fixture.events).unwrap();
    for line in events.lines() {
        let event: Value = serde_json::from_str(line).unwrap();
        if event["kind"] == "release-lock-probe" && event["run_id"] == run_id {
            assert_eq!(
                event["blocked"], true,
                "release flock was acquirable: {event}"
            );
            phases.push(event["phase"].as_str().unwrap().to_string());
        }
    }
    assert_eq!(
        phases,
        vec![
            "merge-abort".to_string(),
            "pinned-checkout".to_string(),
            "worktree-remove".to_string(),
        ],
        "release lock must span restoration through removal for {run_id}"
    );
}

#[test]
fn cad848_changed_merge_result_is_retained_without_force_reset() {
    let _lock = REVIEW_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let output = fixture.review(true, "changed");
    assert!(!output.stdout.is_empty(), "review report was not produced");
    let report: Value = serde_json::from_slice(&output.stdout).expect("review JSON report");
    assert_eq!(report["gated_tree"], "merge-result");
    let report_md = PathBuf::from(report["report_md"].as_str().unwrap());
    assert!(report_md.is_file(), "review Markdown report missing");
    let mut report_json = report_md.clone();
    report_json.set_extension("json");
    assert!(report_json.is_file(), "review JSON report missing");
    assert!(
        report["checkout_lifecycle_warning"].as_str().is_some(),
        "changed checkout refusal was not surfaced: {report}"
    );
    assert!(
        fixture.review_tree.is_dir(),
        "changed review tree was deleted"
    );
    assert!(registered(&fixture.repo, &fixture.review_tree));
    assert_eq!(
        fs::read_to_string(&fixture.changed_marker).unwrap(),
        "external mutation after gates\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.review_tree.join("base-only.txt")).unwrap(),
        "from base\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.review_tree.join("pr-only.txt")).unwrap(),
        "from PR\n"
    );
    assert_eq!(
        git(&fixture.review_tree, &["rev-parse", "HEAD"]),
        fixture.base_sha
    );
    assert_eq!(
        git(
            &fixture.review_tree,
            &["rev-parse", "--verify", "MERGE_HEAD"]
        ),
        fixture.head_sha
    );
    let status = git(
        &fixture.review_tree,
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert!(
        status
            .lines()
            .any(|line| line.starts_with("A ") && line.ends_with("pr-only.txt")),
        "merge-result index changed: {status:?}"
    );
    assert!(status.contains(".cad848-external-change"));

    let record = fixture.record();
    assert_eq!(record["state"], "retained");
    assert_eq!(record["owner"], caller_identity());
    assert_eq!(record["pinned_sha"], fixture.head_sha);
    assert!(!record["retention_reason"].as_str().unwrap_or("").is_empty());

    let events = fs::read_to_string(&fixture.events).unwrap_or_default();
    for line in events.lines() {
        let event: Value = serde_json::from_str(line).unwrap();
        assert_ne!(
            event["destructive"], true,
            "changed checkout was force-reset: {event}"
        );
        assert_ne!(
            event["remove_attempted"], true,
            "changed checkout release attempted: {event}"
        );
    }
    assert!(
        !fixture.probe_path("changed").exists(),
        "changed checkout reached release-time removal"
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "refs/heads/main"]),
        fixture.base_sha
    );
    assert_eq!(
        git(
            &fixture.origin,
            &["rev-parse", &format!("refs/pull/{PR}/head")]
        ),
        fixture.head_sha
    );
}

fn caller_identity() -> String {
    std::env::var("CADENCE_ALIAS").unwrap_or_else(|_| "cadence-review".into())
}

fn assert_review_succeeded(output: &Output) {
    assert!(
        output.status.success(),
        "cadence review failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn registered(repo: &Path, path: &Path) -> bool {
    git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .any(|line| line == format!("worktree {}", path.display()))
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "user.name=CAD-848 fixture",
            "-c",
            "user.email=fixture@invalid",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    let output = cadence_agent::reaper::output(&mut command).expect("run fixture Git");
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

const GIT_OBSERVER: &str = r##"#!/usr/bin/env python3
import fcntl, json, os, subprocess, sys

args = sys.argv[1:]
real = os.environ["CADENCE_CAD848_REAL_GIT"]
root = os.environ["CADENCE_CAD848_ROOT"]
tree = os.environ["CADENCE_CAD848_REVIEW_TREE"]
probe_path = os.environ["CADENCE_CAD848_PROBE"]
events_path = os.environ["CADENCE_CAD848_EVENTS"]
run_id = os.environ["CADENCE_CAD848_RUN_ID"]


def git(*parts):
    return subprocess.run([real, *parts], text=True, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, env=os.environ.copy())


def save_probe(value):
    with open(probe_path, "w", encoding="utf-8") as stream:
        json.dump(value, stream)


def log_event(value):
    with open(events_path, "a", encoding="utf-8") as stream:
        stream.write(json.dumps(value) + "\n")


def require_release_lock(phase):
    lock_path = os.path.join(root, ".cadence", "managed-checkouts.lock")
    blocked = False
    try:
        fd = os.open(lock_path, os.O_RDWR | os.O_CLOEXEC)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                blocked = True
            else:
                fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)
    except OSError:
        blocked = False
    log_event({"kind": "release-lock-probe", "run_id": run_id,
               "phase": phase, "blocked": blocked})
    if not blocked:
        print("managed-checkouts.lock was not held during " + phase, file=sys.stderr)
        sys.exit(95)


def in_tree():
    dirs = [os.getcwd()]
    dirs.extend(args[i + 1] for i, arg in enumerate(args[:-1]) if arg == "-C")
    return any(os.path.realpath(path) == os.path.realpath(tree) for path in dirs)


def destructive():
    if "reset" in args and "--hard" in args:
        return True
    if "clean" in args and any(
        arg == "--force" or (arg.startswith("-") and not arg.startswith("--") and "f" in arg[1:])
        for arg in args
    ):
        return True
    if "checkout" in args and any(arg in ("-f", "--force") for arg in args):
        return True
    if "merge" in args and "--abort" in args:
        return True
    if "checkout" in args and "--detach" in args and os.environ["CADENCE_CAD848_EXPECTED_HEAD"] in args:
        return True
    if "restore" in args and "--worktree" in args:
        return True
    return False


remove = "worktree" in args and "remove" in args and any(
    not arg.startswith("-") and os.path.realpath(arg) == os.path.realpath(tree)
    for arg in args
)
if in_tree() and "merge" in args and "--abort" in args:
    require_release_lock("merge-abort")
if (in_tree() and "checkout" in args and "--detach" in args
        and os.environ["CADENCE_CAD848_EXPECTED_HEAD"] in args):
    require_release_lock("pinned-checkout")
if remove:
    require_release_lock("worktree-remove")

sentinel = os.environ.get("CADENCE_CAD848_TAMPER_SENTINEL", "")
changed = bool(sentinel and os.path.exists(sentinel))
if changed and (in_tree() or remove):
    event = {"args": args, "destructive": destructive(), "remove_attempted": remove}
    log_event(event)
    if destructive() or remove:
        print("refusing destructive operation on externally changed review tree", file=sys.stderr)
        sys.exit(93)

if remove:
    record_path = os.path.join(root, ".cadence", "managed-checkouts.json")
    records = json.load(open(record_path, encoding="utf-8"))["records"]
    record = next((row for row in reversed(records)
                  if os.path.realpath(row["path"]) == os.path.realpath(tree)), None)
    artifacts = record.get("release_artifacts", []) if record else []
    md = next((path for path in artifacts if path.endswith(".md")), "")
    report_path = next((path for path in artifacts if path.endswith(".json")), "")
    report_matches = False
    if report_path and os.path.isfile(report_path):
        report = json.load(open(report_path, encoding="utf-8"))
        report_matches = report.get("report_md") == md and report.get("head") == os.environ["CADENCE_CAD848_EXPECTED_HEAD"]
    head = git("-C", tree, "rev-parse", "HEAD").stdout.strip()
    status = git("-C", tree, "status", "--porcelain", "--untracked-files=all")
    detached = git("-C", tree, "symbolic-ref", "--quiet", "--short", "HEAD").returncode == 1
    actual = {
        "run_id": run_id,
        "head": head,
        "clean": status.returncode == 0 and not status.stdout,
        "detached": detached,
        "record_state": record.get("state") if record else None,
        "artifacts_recorded": len(artifacts) == 2 and bool(md) and bool(report_path),
        "artifacts_exist": len(artifacts) == 2 and all(os.path.isfile(path) and os.path.getsize(path) > 0 for path in artifacts),
        "artifacts_outside_tree": len(artifacts) == 2 and all(
            os.path.commonpath([os.path.realpath(tree), os.path.realpath(path)]) != os.path.realpath(tree)
            for path in artifacts
        ),
        "report_matches": report_matches,
        "removed": False,
    }
    actual["ready"] = (
        actual["head"] == os.environ["CADENCE_CAD848_EXPECTED_HEAD"]
        and actual["clean"] and actual["detached"]
        and actual["record_state"] == "releasing"
        and actual["artifacts_recorded"] and actual["artifacts_exist"]
        and actual["artifacts_outside_tree"] and actual["report_matches"]
        and not any(arg in ("--force", "-f") for arg in args)
    )
    save_probe(actual)
    if not actual["ready"]:
        print("release attempted before exact-head/artifact preconditions: " + json.dumps(actual), file=sys.stderr)
        sys.exit(94)
    result = subprocess.run([real, *args], env=os.environ.copy())
    actual["removed"] = result.returncode == 0 and not os.path.exists(tree)
    save_probe(actual)
    sys.exit(result.returncode)

os.execvpe(real, [real, *args], os.environ.copy())
"##;
