//! CAD-848 independent acceptance check for fail-closed target reclamation.
//!
//! Run with `--features test-seam`: process outcomes use explicit refusal
//! inputs or isolated proc-tree fixtures, while the real reclaim path selects
//! the lane, applies live-use/idle guards, and performs cache deletion. Each
//! fixture has a private PM, repo, unmerged lane and target sentinel.

#![cfg(feature = "test-seam")]

use cadence_agent::issue::{
    reclaim::{self, ReclaimTestProcessUse},
    start::{self, StartArgs},
    write, Pm,
};
use cadence_agent::worktree::lifecycle;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
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
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
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

    fn reclaim_from_proc_root(&self, proc_root: &Path) -> Value {
        self.reclaim(ReclaimTestProcessUse::ScanProcRoot(proc_root.to_path_buf()))
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

struct FakeCredentials<'a> {
    uid: u32,
    gid: u32,
    groups: &'a [u32],
    cap_eff: u64,
}

fn write_proc_mountinfo(proc_root: &Path, hidepid: Option<u8>) {
    fs::create_dir_all(proc_root.join("self")).unwrap();
    let super_options =
        hidepid.map_or_else(|| "rw".to_string(), |value| format!("rw,hidepid={value}"));
    fs::write(
        proc_root.join("self/mountinfo"),
        format!(
            "29 23 0:55 / {} rw,nosuid,nodev,noexec,relatime - proc proc {super_options}\n31 23 0:5 net:[4026532471] /run/cad848-netns rw - nsfs nsfs rw\n",
            proc_root.canonicalize().unwrap().display()
        ),
    )
    .unwrap();
}

fn fake_process(
    proc_root: &Path,
    pid: u32,
    credentials: FakeCredentials<'_>,
    cwd: &Path,
    fds: &[(&str, &Path)],
) -> PathBuf {
    fake_process_with_state(proc_root, pid, credentials, "S", Some(cwd), fds)
}

fn fake_process_with_state(
    proc_root: &Path,
    pid: u32,
    credentials: FakeCredentials<'_>,
    state: &str,
    cwd: Option<&Path>,
    fds: &[(&str, &Path)],
) -> PathBuf {
    let proc_dir = proc_root.join(pid.to_string());
    fs::create_dir_all(&proc_dir).unwrap();
    let groups = credentials
        .groups
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("\t");
    fs::write(
        proc_dir.join("status"),
        format!(
            "Name:\tfixture\nState:\t{state} (fixture)\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t{gid}\t{gid}\t{gid}\t{gid}\nGroups:\t{groups}\nCapEff:\t{cap_eff:016x}\n",
            uid = credentials.uid,
            gid = credentials.gid,
            cap_eff = credentials.cap_eff,
        ),
    )
    .unwrap();
    if let Some(cwd) = cwd {
        symlink(cwd, proc_dir.join("cwd")).unwrap();
        let fd_dir = proc_dir.join("fd");
        fs::create_dir_all(&fd_dir).unwrap();
        for (fd, target) in fds {
            symlink(target, fd_dir.join(fd)).unwrap();
        }
    }
    proc_dir
}

fn foreign_uid(owner_uid: u32) -> u32 {
    if owner_uid == u32::MAX {
        owner_uid - 1
    } else {
        owner_uid + 1
    }
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
    assert!(
        process_lane.join("unmerged.txt").is_file(),
        "process uncertainty deleted source"
    );
    assert_registration(
        &process_fixture,
        &process_lane,
        &process_branch,
        &process_tip,
    );
    let process_ledger_path = process_fixture.repo.join(".cadence/managed-checkouts.json");
    let process_ledger_before = fs::read(&process_ledger_path).unwrap();
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
    assert_eq!(
        fs::read(&process_ledger_path).unwrap(),
        process_ledger_before,
        "target-cache reclaim changed lifecycle metadata"
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
    assert!(
        walk_lane.join("unmerged.txt").is_file(),
        "incomplete idle walk deleted source"
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

    // This same-owner fake process holds a lane FD, but its fd directory is
    // inaccessible. Refusal must name that process enumeration failure.
    let owner_fixture = Fixture::new("owner uid inaccessible fd holder");
    let (owner_lane, owner_branch, owner_tip) = owner_fixture.start_unmerged_lane();
    let owner_target = target_with_sentinel(&owner_lane);
    let owner_sentinel = owner_target.join(TARGET_SENTINEL);
    let owner_held_file = owner_target.join("owner-inaccessible-fd");
    fs::write(&owner_held_file, b"CAD848-OWNER-INACCESSIBLE-FD\n").unwrap();
    let owner_metadata = fs::metadata(&owner_lane).unwrap();
    let owner_uid = owner_metadata.uid();
    let owner_gid = owner_metadata.gid();
    let owner_proc_root = Builder::new()
        .prefix("c848proc-owner-")
        .tempdir_in("/tmp")
        .unwrap();
    write_proc_mountinfo(owner_proc_root.path(), None);
    let owner_elsewhere = owner_proc_root.path().join("elsewhere");
    fs::create_dir(&owner_elsewhere).unwrap();
    let owner_pid = u32::MAX - 2;
    let owner_proc = fake_process(
        owner_proc_root.path(),
        owner_pid,
        FakeCredentials {
            uid: owner_uid,
            gid: owner_gid,
            groups: &[],
            cap_eff: 0,
        },
        &owner_elsewhere,
        &[("9", &owner_held_file)],
    );
    let owner_fd_dir = owner_proc.join("fd");
    assert_eq!(
        fs::read_link(owner_fd_dir.join("9")).unwrap(),
        owner_held_file.canonicalize().unwrap(),
        "owner fixture must hold a lane FD before its proc fd scan is denied"
    );
    fs::set_permissions(&owner_fd_dir, fs::Permissions::from_mode(0o0)).unwrap();
    let owner_fd_error = match fs::read_dir(&owner_fd_dir) {
        Ok(_) => panic!("owner-holder fd fixture unexpectedly readable"),
        Err(error) => error,
    };
    assert_eq!(
        owner_fd_error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "owner-holder fixture must produce a real fd PermissionDenied"
    );
    let restore_owner_fd_permissions = RestorePermissions(owner_fd_dir);
    let result = owner_fixture.reclaim_from_proc_root(owner_proc_root.path());
    drop(restore_owner_fd_permissions);
    let reason = refusal_reason(&result, "Cannot fully enumerate process cwd/open-fd use");
    assert!(
        reason.contains(&format!("file descriptors for process {owner_pid}"))
            && reason.contains("Permission denied"),
        "owner-holder scan error did not identify the refusing PID: {reason}"
    );
    assert_eq!(fs::read(&owner_sentinel).unwrap(), TARGET_CONTENT);
    assert!(
        owner_lane.join("unmerged.txt").is_file(),
        "owner EACCES deleted source"
    );
    assert_registration(&owner_fixture, &owner_lane, &owner_branch, &owner_tip);

    // A foreign UID cannot traverse the private root, but an inaccessible FD
    // scan cannot prove it lacks an inherited or transferred lane descriptor.
    let foreign_fixture = Fixture::new("foreign uid inaccessible fd");
    let (foreign_lane, foreign_branch, foreign_tip) = foreign_fixture.start_unmerged_lane();
    let foreign_target = target_with_sentinel(&foreign_lane);
    let foreign_held_file = foreign_target.join("foreign-inaccessible-fd");
    fs::write(&foreign_held_file, b"CAD848-FOREIGN-INACCESSIBLE-FD\n").unwrap();
    let foreign_metadata = fs::metadata(&foreign_lane).unwrap();
    let foreign_process_uid = foreign_uid(foreign_metadata.uid());
    let foreign_process_gid = foreign_uid(foreign_metadata.gid());
    assert_eq!(
        fs::metadata(foreign_fixture._root.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "foreign UID must be unable to traverse the private fixture root"
    );
    let foreign_proc_root = Builder::new()
        .prefix("c848proc-foreign-")
        .tempdir_in("/tmp")
        .unwrap();
    write_proc_mountinfo(foreign_proc_root.path(), None);
    let foreign_elsewhere = foreign_proc_root.path().join("elsewhere");
    fs::create_dir(&foreign_elsewhere).unwrap();
    let foreign_pid = u32::MAX - 1;
    let foreign_proc = fake_process(
        foreign_proc_root.path(),
        foreign_pid,
        FakeCredentials {
            uid: foreign_process_uid,
            gid: foreign_process_gid,
            groups: &[],
            cap_eff: 0,
        },
        &foreign_elsewhere,
        &[("9", &foreign_held_file)],
    );
    let foreign_fd_dir = foreign_proc.join("fd");
    assert_eq!(
        fs::read_link(foreign_fd_dir.join("9")).unwrap(),
        foreign_held_file.canonicalize().unwrap(),
        "foreign fixture must hold a lane FD before its proc fd scan is denied"
    );
    fs::set_permissions(&foreign_fd_dir, fs::Permissions::from_mode(0o0)).unwrap();
    let fd_error = match fs::read_dir(&foreign_fd_dir) {
        Ok(_) => panic!("foreign fd fixture unexpectedly readable"),
        Err(error) => error,
    };
    assert_eq!(
        fd_error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "fixture must produce a real foreign-fd PermissionDenied"
    );
    let restore_fd_permissions = RestorePermissions(foreign_fd_dir);
    let result = foreign_fixture.reclaim_from_proc_root(foreign_proc_root.path());
    drop(restore_fd_permissions);
    let reason = refusal_reason(&result, "Cannot fully enumerate process cwd/open-fd use");
    assert!(
        reason.contains(&format!("process {foreign_pid}")) && reason.contains("Permission denied"),
        "foreign inaccessible-FD error was not the fail-closed cause: {reason}"
    );
    assert_eq!(
        fs::read(foreign_target.join(TARGET_SENTINEL)).unwrap(),
        TARGET_CONTENT
    );
    assert!(foreign_target.is_dir(), "foreign EACCES deleted target");
    assert!(
        foreign_lane.join("unmerged.txt").is_file(),
        "foreign EACCES deleted source"
    );
    assert_registration(
        &foreign_fixture,
        &foreign_lane,
        &foreign_branch,
        &foreign_tip,
    );

    // A root-UID holder can also have inherited a descriptor even when the
    // private checkout path is not traversable. Its denied FD scan must refuse.
    let root_fixture = Fixture::new("root uid inaccessible fd");
    let (root_lane, root_branch, root_tip) = root_fixture.start_unmerged_lane();
    let root_target = target_with_sentinel(&root_lane);
    let root_sentinel = root_target.join(TARGET_SENTINEL);
    let root_held_file = root_target.join("root-inaccessible-fd");
    fs::write(&root_held_file, b"CAD848-ROOT-INACCESSIBLE-FD\n").unwrap();
    let root_proc_root = Builder::new()
        .prefix("c848proc-root-")
        .tempdir_in("/tmp")
        .unwrap();
    write_proc_mountinfo(root_proc_root.path(), None);
    let root_elsewhere = root_proc_root.path().join("elsewhere");
    fs::create_dir(&root_elsewhere).unwrap();
    let root_pid = u32::MAX - 4;
    let root_proc = fake_process(
        root_proc_root.path(),
        root_pid,
        FakeCredentials {
            uid: 0,
            gid: 0,
            groups: &[],
            cap_eff: 0,
        },
        &root_elsewhere,
        &[("9", &root_held_file)],
    );
    let root_fd_dir = root_proc.join("fd");
    assert_eq!(
        fs::read_link(root_fd_dir.join("9")).unwrap(),
        root_held_file.canonicalize().unwrap(),
        "root fixture must hold a lane FD before its proc fd scan is denied"
    );
    fs::set_permissions(&root_fd_dir, fs::Permissions::from_mode(0o0)).unwrap();
    let root_fd_error = match fs::read_dir(&root_fd_dir) {
        Ok(_) => panic!("root-holder fd fixture unexpectedly readable"),
        Err(error) => error,
    };
    assert_eq!(
        root_fd_error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "root-holder fixture must produce a real fd PermissionDenied"
    );
    let restore_root_fd_permissions = RestorePermissions(root_fd_dir);
    let result = root_fixture.reclaim_from_proc_root(root_proc_root.path());
    drop(restore_root_fd_permissions);
    let reason = refusal_reason(&result, "Cannot fully enumerate process cwd/open-fd use");
    assert!(
        reason.contains(&format!("process {root_pid}")) && reason.contains("Permission denied"),
        "root-holder FD scan error was not fail-closed: {reason}"
    );
    assert_eq!(fs::read(&root_sentinel).unwrap(), TARGET_CONTENT);
    assert!(
        root_lane.join("unmerged.txt").is_file(),
        "root EACCES deleted source"
    );
    assert_registration(&root_fixture, &root_lane, &root_branch, &root_tip);

    // A foreign UID with the checkout group can traverse the temp-root path.
    // A denied FD scan must therefore remain fail-closed rather than being
    // classified as unrelated.
    let group_fixture = Fixture::new("foreign uid with checkout group");
    let (group_lane, group_branch, group_tip) = group_fixture.start_unmerged_lane();
    let group_target = target_with_sentinel(&group_lane);
    let group_sentinel = group_target.join(TARGET_SENTINEL);
    let group_root = group_fixture._root.path();
    let group_root_metadata = fs::metadata(group_root).unwrap();
    let checkout_gid = group_root_metadata.gid();
    fs::set_permissions(group_root, fs::Permissions::from_mode(0o710)).unwrap();
    assert_ne!(
        fs::metadata(group_root).unwrap().permissions().mode() & 0o010,
        0,
        "group path component must permit traversal"
    );
    let group_uid = foreign_uid(group_root_metadata.uid());
    let group_primary_gid = foreign_uid(checkout_gid);
    let group_proc_root = Builder::new()
        .prefix("c848proc-group-")
        .tempdir_in("/tmp")
        .unwrap();
    write_proc_mountinfo(group_proc_root.path(), None);
    let group_elsewhere = group_proc_root.path().join("elsewhere");
    fs::create_dir(&group_elsewhere).unwrap();
    let group_proc = fake_process(
        group_proc_root.path(),
        u32::MAX - 3,
        FakeCredentials {
            uid: group_uid,
            gid: group_primary_gid,
            groups: &[checkout_gid],
            cap_eff: 0,
        },
        &group_elsewhere,
        &[],
    );
    let group_fd_dir = group_proc.join("fd");
    fs::set_permissions(&group_fd_dir, fs::Permissions::from_mode(0o0)).unwrap();
    let group_fd_error = match fs::read_dir(&group_fd_dir) {
        Ok(_) => panic!("group-access fd fixture unexpectedly readable"),
        Err(error) => error,
    };
    assert_eq!(
        group_fd_error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "group fixture must produce a real fd PermissionDenied"
    );
    let restore_group_fd_permissions = RestorePermissions(group_fd_dir);
    let result = group_fixture.reclaim_from_proc_root(group_proc_root.path());
    drop(restore_group_fd_permissions);
    let reason = refusal_reason(&result, "Cannot fully enumerate process cwd/open-fd use");
    assert!(
        reason.contains(&format!("process {}", u32::MAX - 3))
            && reason.contains("Permission denied"),
        "group-traversable foreign process was not fail-closed: {reason}"
    );
    assert_eq!(fs::read(&group_sentinel).unwrap(), TARGET_CONTENT);
    assert!(
        group_lane.join("unmerged.txt").is_file(),
        "group EACCES deleted source"
    );
    assert_registration(&group_fixture, &group_lane, &group_branch, &group_tip);

    // A visible foreign-UID process is still checked. Its cwd is elsewhere,
    // but a visible fd symlink into the lane must retain the target.
    let visible_fixture = Fixture::new("visible foreign fd holder");
    let (visible_lane, visible_branch, visible_tip) = visible_fixture.start_unmerged_lane();
    let visible_target = target_with_sentinel(&visible_lane);
    let visible_sentinel = visible_target.join(TARGET_SENTINEL);
    let held_file = visible_target.join("foreign-open-file");
    fs::write(&held_file, b"CAD848-FOREIGN-FD-HOLD\n").unwrap();
    let visible_root = visible_fixture._root.path();
    let visible_root_metadata = fs::metadata(visible_root).unwrap();
    let visible_group = visible_root_metadata.gid();
    fs::set_permissions(visible_root, fs::Permissions::from_mode(0o710)).unwrap();
    let visible_uid = foreign_uid(visible_root_metadata.uid());
    let visible_primary_gid = foreign_uid(visible_group);
    let visible_groups = [visible_group];
    let visible_proc_root = Builder::new()
        .prefix("c848proc-visible-")
        .tempdir_in("/tmp")
        .unwrap();
    write_proc_mountinfo(visible_proc_root.path(), None);
    let visible_elsewhere = visible_proc_root.path().join("elsewhere");
    fs::create_dir(&visible_elsewhere).unwrap();
    let visible_pid = u32::MAX;
    fake_process(
        visible_proc_root.path(),
        visible_pid,
        FakeCredentials {
            uid: visible_uid,
            gid: visible_primary_gid,
            groups: &visible_groups,
            cap_eff: 0,
        },
        &visible_elsewhere,
        &[("9", &held_file)],
    );
    let result = visible_fixture.reclaim_from_proc_root(visible_proc_root.path());
    let reason = refusal_reason(&result, &format!("Process {visible_pid}"));
    assert!(
        reason.contains("holds an open file descriptor inside"),
        "visible foreign FD did not trigger the open-FD guard: {reason}"
    );
    assert_eq!(fs::read(&visible_sentinel).unwrap(), TARGET_CONTENT);
    assert!(
        visible_lane.join("unmerged.txt").is_file(),
        "visible FD holder deleted source"
    );
    assert_registration(
        &visible_fixture,
        &visible_lane,
        &visible_branch,
        &visible_tip,
    );

    let hidden_fixture = Fixture::new("hidepid omits other uid");
    let (hidden_lane, hidden_branch, hidden_tip) = hidden_fixture.start_unmerged_lane();
    let hidden_target = target_with_sentinel(&hidden_lane);
    let hidden_sentinel = hidden_target.join(TARGET_SENTINEL);
    let hidden_proc_root = Builder::new()
        .prefix("c848proc-hidepid-")
        .tempdir_in("/tmp")
        .unwrap();
    write_proc_mountinfo(hidden_proc_root.path(), Some(2));
    assert!(fs::read_dir(hidden_proc_root.path())
        .unwrap()
        .all(|entry| entry.unwrap().file_name() == "self"));
    let result = hidden_fixture.reclaim_from_proc_root(hidden_proc_root.path());
    let reason = refusal_reason(&result, "Cannot fully enumerate process cwd/open-fd use");
    assert!(
        reason.to_ascii_lowercase().contains("hidepid"),
        "restricted PID visibility was not the refusal reason: {reason}"
    );
    assert_eq!(fs::read(&hidden_sentinel).unwrap(), TARGET_CONTENT);
    assert!(hidden_target.is_dir());
    assert!(hidden_lane.join("unmerged.txt").is_file());
    assert_registration(&hidden_fixture, &hidden_lane, &hidden_branch, &hidden_tip);

    let visibility_fixture = Fixture::new("missing proc mountinfo");
    let (visibility_lane, visibility_branch, visibility_tip) =
        visibility_fixture.start_unmerged_lane();
    let visibility_target = target_with_sentinel(&visibility_lane);
    let visibility_sentinel = visibility_target.join(TARGET_SENTINEL);
    let missing_proc_root = Builder::new()
        .prefix("c848proc-no-mountinfo-")
        .tempdir_in("/tmp")
        .unwrap();
    let missing = visibility_fixture.reclaim_from_proc_root(missing_proc_root.path());
    let missing_reason = refusal_reason(&missing, "Cannot fully enumerate process cwd/open-fd use");
    assert!(
        missing_reason.to_ascii_lowercase().contains("mountinfo"),
        "missing visibility metadata was not identified: {missing_reason}"
    );
    assert_eq!(fs::read(&visibility_sentinel).unwrap(), TARGET_CONTENT);
    assert!(
        visibility_lane.join("unmerged.txt").is_file(),
        "missing mountinfo deleted source"
    );
    assert_registration(
        &visibility_fixture,
        &visibility_lane,
        &visibility_branch,
        &visibility_tip,
    );

    let malformed_proc_root = Builder::new()
        .prefix("c848proc-malformed-mountinfo-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::create_dir_all(malformed_proc_root.path().join("self")).unwrap();
    fs::write(
        malformed_proc_root.path().join("self/mountinfo"),
        "not a mountinfo record\n",
    )
    .unwrap();
    let malformed = visibility_fixture.reclaim_from_proc_root(malformed_proc_root.path());
    let malformed_reason =
        refusal_reason(&malformed, "Cannot fully enumerate process cwd/open-fd use");
    assert!(
        malformed_reason.to_ascii_lowercase().contains("mountinfo"),
        "malformed visibility metadata was not identified: {malformed_reason}"
    );
    assert_eq!(fs::read(&visibility_sentinel).unwrap(), TARGET_CONTENT);
    assert!(visibility_target.is_dir());
    assert!(visibility_lane.join("unmerged.txt").is_file());
    assert_registration(
        &visibility_fixture,
        &visibility_lane,
        &visibility_branch,
        &visibility_tip,
    );

    let invalid_escape_proc_root = Builder::new()
        .prefix("c848proc-invalid-mountinfo-escape-")
        .tempdir_in("/tmp")
        .unwrap();
    write_proc_mountinfo(invalid_escape_proc_root.path(), None);
    let mountinfo_path = invalid_escape_proc_root.path().join("self/mountinfo");
    let mut mountinfo = fs::read_to_string(&mountinfo_path).unwrap();
    mountinfo.push_str(
        r"30 23 0:56 / /mnt/cad848\440-unrelated rw,nosuid,nodev,noexec,relatime - ext4 /dev/test rw
",
    );
    fs::write(&mountinfo_path, mountinfo).unwrap();
    let invalid_escape = visibility_fixture.reclaim_from_proc_root(invalid_escape_proc_root.path());
    let invalid_escape_reason = refusal_reason(
        &invalid_escape,
        "Cannot fully enumerate process cwd/open-fd use",
    );
    assert!(
        invalid_escape_reason
            .to_ascii_lowercase()
            .contains("mountinfo"),
        "invalid octal escape was accepted as a mountinfo path: {invalid_escape_reason}"
    );
    assert_eq!(fs::read(&visibility_sentinel).unwrap(), TARGET_CONTENT);
    assert!(visibility_target.is_dir());
    assert!(visibility_lane.join("unmerged.txt").is_file());
    assert_registration(
        &visibility_fixture,
        &visibility_lane,
        &visibility_branch,
        &visibility_tip,
    );

    for state in ["Z", "X"] {
        let dead_fixture = Fixture::new(&format!("known-dead {state} process"));
        let (dead_lane, dead_branch, dead_tip) = dead_fixture.start_unmerged_lane();
        let dead_target = target_with_sentinel(&dead_lane);
        let dead_proc_root = Builder::new()
            .prefix("c848proc-dead-")
            .tempdir_in("/tmp")
            .unwrap();
        write_proc_mountinfo(dead_proc_root.path(), None);
        fake_process_with_state(
            dead_proc_root.path(),
            u32::MAX - 10,
            FakeCredentials {
                uid: 0,
                gid: 0,
                groups: &[],
                cap_eff: 0,
            },
            state,
            None,
            &[],
        );
        let result = dead_fixture.reclaim_from_proc_root(dead_proc_root.path());
        assert_reclaimed(&result);
        assert!(
            !dead_target.exists(),
            "known-dead {state} process blocked cleanup"
        );
        assert!(dead_lane.join("unmerged.txt").is_file());
        assert_registration(&dead_fixture, &dead_lane, &dead_branch, &dead_tip);
    }

    let live_fixture = Fixture::new("live state with unavailable proc entries");
    let (live_lane, live_branch, live_tip) = live_fixture.start_unmerged_lane();
    let live_target = target_with_sentinel(&live_lane);
    let live_sentinel = live_target.join(TARGET_SENTINEL);
    let live_proc_root = Builder::new()
        .prefix("c848proc-live-unknown-")
        .tempdir_in("/tmp")
        .unwrap();
    write_proc_mountinfo(live_proc_root.path(), None);
    fake_process_with_state(
        live_proc_root.path(),
        u32::MAX - 11,
        FakeCredentials {
            uid: 0,
            gid: 0,
            groups: &[],
            cap_eff: 0,
        },
        "S",
        None,
        &[],
    );
    let result = live_fixture.reclaim_from_proc_root(live_proc_root.path());
    let reason = refusal_reason(&result, "Cannot fully enumerate process cwd/open-fd use");
    assert!(
        reason.contains(&format!("process {}", u32::MAX - 11)),
        "live process with missing cwd/fd was not identified: {reason}"
    );
    assert_eq!(fs::read(&live_sentinel).unwrap(), TARGET_CONTENT);
    assert!(live_target.is_dir());
    assert!(
        live_lane.join("unmerged.txt").is_file(),
        "unavailable live proc entries deleted source"
    );
    assert_registration(&live_fixture, &live_lane, &live_branch, &live_tip);

    let retained_fixture = Fixture::new("retained target refusal");
    let (retained_lane, retained_branch, retained_tip) = retained_fixture.start_unmerged_lane();
    let retained_target = target_with_sentinel(&retained_lane);
    let retained_sentinel = retained_target.join(TARGET_SENTINEL);
    let retained_reason = "CAD-848 preserve retained lane target";
    lifecycle::transition(
        &retained_fixture.repo,
        &retained_lane,
        "retained",
        Some(retained_reason),
    )
    .expect("record retained lifecycle state");
    let ledger_path = retained_fixture
        .repo
        .join(".cadence/managed-checkouts.json");
    let issue_path = retained_fixture
        .pm
        .dir
        .join(PROJECT)
        .join(ISSUE)
        .join("issue.md");
    let ledger_before = fs::read(&ledger_path).unwrap();
    let issue_before = fs::read(&issue_path).unwrap();
    let branch_before = git(
        &retained_fixture.home,
        &retained_fixture.repo,
        &["rev-parse", &retained_branch],
    );
    let plan = reclaim::plan_for_repo(
        &retained_fixture.pm,
        &retained_fixture.state,
        IDLE_SECS,
        &retained_fixture.repo,
    )
    .expect("read-only reclaim plan");
    let plan_row = plan["cache_resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["resource"]["issue"].as_str() == Some(ISSUE))
        .expect("retained lane cache plan row");
    assert_ne!(plan_row["status"], "reclaimable-cache-only", "{plan_row}");
    assert!(
        plan_row["reason"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("retained"),
        "plan did not identify retained lifecycle state: {plan_row}"
    );
    assert!(plan_row["reclaimable_bytes"].is_null(), "{plan_row}");
    assert_eq!(fs::read(&ledger_path).unwrap(), ledger_before);
    assert_eq!(fs::read(&issue_path).unwrap(), issue_before);

    let result = retained_fixture.reclaim(ReclaimTestProcessUse::CompleteNoUse);
    let refused = result["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["issue"].as_str() == Some(ISSUE))
        .expect("retained target reported as refused");
    assert!(
        refused["reason"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("retained"),
        "scheduled refusal did not identify retained lifecycle state: {refused}"
    );
    assert!(result["reclaimed"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["issue"].as_str() != Some(ISSUE)));
    assert_eq!(fs::read(&retained_sentinel).unwrap(), TARGET_CONTENT);
    assert!(retained_target.is_dir());
    assert!(retained_lane.join("unmerged.txt").is_file());
    assert_eq!(fs::read(&ledger_path).unwrap(), ledger_before);
    assert_eq!(fs::read(&issue_path).unwrap(), issue_before);
    assert_eq!(
        git(
            &retained_fixture.home,
            &retained_fixture.repo,
            &["rev-parse", &retained_branch],
        ),
        branch_before
    );
    assert_registration(
        &retained_fixture,
        &retained_lane,
        &retained_branch,
        &retained_tip,
    );
}
