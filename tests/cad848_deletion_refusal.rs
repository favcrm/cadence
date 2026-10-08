//! CAD-848 independent deletion-refusal acceptance check.
//!
//! Exercise the real `cadence issue finish` checkout-deletion path, not a
//! copied predicate: a managed, merged, clean lane must survive an open FD
//! held by a process whose cwd is elsewhere; a foreign/unadopted checkout
//! must remain inventory/history, not be removed merely because an issue
//! ref points at it. Each fixture, PM, repository, and process is owned by
//! this test under a short `/tmp` root. Refusal checks include actionable
//! reason categories and sentinels plus Git registrations prove no deletion.
//!
//! Integrate unchanged as `tests/cad848_deletion_refusal.rs`. The managed
//! fixture uses `issue::start::run` so any supported ownership metadata is
//! created through the real setup path. The foreign fixture is a linked
//! checkout of a different, undeclared repo with no Cadence registration;
//! its synthetic issue ref is only the candidate presented to the real
//! finish guard, not adoption.

use cadence_agent::issue::{
    model::Ref,
    parse,
    start::{self, StartArgs},
    write, Pm,
};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const TEST: &str = "cad848_finish_refuses_open_fd_and_foreign_ownership";
const ISOLATED: &str = "CADENCE_CAD848_GUARD_ISOLATED";
const PROJECT: &str = "guard";
const PREFIX: &str = "C84";
const ACTOR: &str = "guard-author";

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    state: PathBuf,
    pm: Pm,
    repo: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = Builder::new()
            .prefix("c848guard-")
            .tempdir_in("/tmp")
            .expect("isolated fixture root");
        let home = root.path().join("h");
        let state = root.path().join("s");
        let repo = root.path().join("repo");
        fs::create_dir_all(&home).unwrap();
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
        let fx = Self {
            _root: root,
            home,
            state,
            pm,
            repo,
        };
        fx.new_issue(&format!("{tag} fixture"));
        fx
    }

    fn new_issue(&self, title: &str) {
        let id = issue_id(title);
        write::new_issue(
            &self.pm,
            &self.repo,
            Some(PROJECT),
            title,
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some(&id),
            Some("Isolated cleanup refusal fixture."),
            ACTOR,
        )
        .expect("fixture issue");
    }

    fn start_managed_lane(&self, id: &str) -> (PathBuf, String) {
        let result = start::run(
            &self.pm,
            id,
            &StartArgs {
                repo: Some(self.repo.clone()),
                name: Some("guard".to_string()),
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
        (
            PathBuf::from(result["worktree"].as_str().unwrap()),
            result["branch"].as_str().unwrap().to_string(),
        )
    }

    fn finish(&self, id: &str, lane: &Path) -> Output {
        let mut cmd = Command::new(BINARY);
        cmd.args([
            "issue",
            "finish",
            id,
            "--worktree",
            lane.to_str().expect("UTF-8 fixture path"),
        ])
        .env("CADENCE_PM_DIR", &self.pm.dir)
        .env("HOME", &self.home)
        .env("XDG_STATE_HOME", &self.state)
        .env("XDG_CONFIG_HOME", self.home.join("config"))
        .env("XDG_DATA_HOME", self.home.join("data"))
        .env("XDG_CACHE_HOME", self.home.join("cache"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR");
        cadence_agent::reaper::output(&mut cmd).expect("run real issue finish CLI")
    }
}

fn issue_id(tag: &str) -> String {
    let n = if tag.starts_with("foreign") { 2 } else { 1 };
    format!("{PREFIX}-{n}")
}

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=CAD-848 guard",
        "-c",
        "user.email=guard@invalid",
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

fn commit_and_merge(home: &Path, repo: &Path, lane: &Path, branch: &str, marker: &str) {
    fs::write(lane.join("work.txt"), marker).unwrap();
    git(home, lane, &["add", "work.txt"]);
    git(home, lane, &["commit", "--quiet", "-m", "fixture work"]);
    git(home, repo, &["merge", "--quiet", "--ff-only", branch]);
    git(home, repo, &["push", "--quiet", "origin", "main"]);
    git(home, repo, &["fetch", "--quiet", "origin"]);
    age_tracked_files(lane);
}

fn age_tracked_files(lane: &Path) {
    let then = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(2 * 60 * 60);
    for name in [".gitignore", "tracked.txt", "work.txt"] {
        let mut cmd = Command::new("touch");
        cmd.args(["-h", "-d", &format!("@{then}")])
            .arg(lane.join(name));
        let out = cadence_agent::reaper::output(&mut cmd).expect("age fixture source");
        assert!(out.status.success(), "touch failed: {}", name);
    }
}

fn add_refs(pm: &Pm, id: &str, lane: &Path, branch: &str) {
    let file = pm.dir.join(PROJECT).join(id).join("issue.md");
    let text = fs::read_to_string(&file).unwrap();
    let (mut front, body) = parse::parse_issue(&text).unwrap();
    front.refs = vec![
        Ref {
            kind: "worktree".to_string(),
            url: None,
            path: Some(lane.display().to_string()),
            label: None,
            closed: None,
            worktree: None,
            cargo_target: None,
            agent: None,
        },
        Ref {
            kind: "branch".to_string(),
            url: None,
            path: Some(branch.to_string()),
            label: None,
            closed: None,
            worktree: None,
            cargo_target: None,
            agent: None,
        },
    ];
    fs::write(&file, parse::render(&front, &body).unwrap()).unwrap();
}

fn refusal_text(out: &Output, expected: &[&str]) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "finish unexpectedly succeeded: {text}"
    );
    let lower = text.to_ascii_lowercase();
    assert!(
        expected.iter().any(|part| lower.contains(part)),
        "refusal did not identify the guarded condition: {text}"
    );
}

fn refusal_names_fd_holder(out: &Output, pid: u32, lane: &Path) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "finish unexpectedly succeeded with FD holder {pid}: {text}"
    );
    let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
        .expect("read fixture FD holder process name")
        .trim()
        .to_string();
    let finding = format!(
        "process {pid} ({comm}) holds an open file descriptor inside {}",
        lane.display()
    )
    .to_ascii_lowercase();
    assert!(
        text.to_ascii_lowercase().contains(&finding),
        "finish refusal did not attribute open-FD guard to fixture PID {pid}; expected {finding:?}: {text}"
    );
}

struct FdHolder {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl FdHolder {
    fn open_elsewhere(file: &Path, cwd: &Path) -> Self {
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "exec 9<\"$1\"; printf 'ready\\n'; IFS= read -r _",
                "cad848-fd-holder",
                file.to_str().unwrap(),
            ])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            cadence_agent::reaper::spawn(&mut command).expect("start owned FD fixture process");
        let stdin = child.stdin.take().unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .expect("read FD fixture readiness");
        assert_eq!(ready.trim(), "ready", "FD fixture failed to start");
        let pid = child.id();
        assert_eq!(
            fs::read_link(format!("/proc/{pid}/cwd")).unwrap(),
            cwd.canonicalize().unwrap(),
            "FD holder cwd must be outside the candidate"
        );
        assert_eq!(
            fs::read_link(format!("/proc/{pid}/fd/9")).unwrap(),
            file.canonicalize().unwrap(),
            "FD holder must retain the candidate sentinel"
        );
        Self {
            child,
            stdin: Some(stdin),
        }
    }
}

impl Drop for FdHolder {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[test]
fn cad848_finish_refuses_open_fd_and_foreign_ownership() {
    if std::env::var_os(ISOLATED).is_none() {
        let sandbox = Builder::new()
            .prefix("c848guard-run-")
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

    // Active use: issue start creates the real managed lane; the work is
    // merged and all tracked files aged, so the ONLY live-use evidence is
    // an open descriptor in a child whose cwd we prove is elsewhere.
    let managed = Fixture::new("managed");
    let id = "C84-1";
    let (lane, branch) = managed.start_managed_lane(id);
    commit_and_merge(
        &managed.home,
        &managed.repo,
        &lane,
        &branch,
        "managed work\n",
    );
    let sentinel = lane.join(".cad848-keep-sentinel");
    fs::write(&sentinel, b"CAD848-MANAGED-KEEP\n").unwrap();
    let exclude = PathBuf::from(git(
        &managed.home,
        &lane,
        &["rev-parse", "--git-path", "info/exclude"],
    ));
    let exclude = if exclude.is_absolute() {
        exclude
    } else {
        lane.join(exclude)
    };
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(exclude)
        .unwrap()
        .write_all(b"/.cad848-keep-sentinel\n")
        .unwrap();
    let elsewhere = managed._root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let mut holder = FdHolder::open_elsewhere(&sentinel, &elsewhere);
    let holder_pid = holder.child.id();
    let result = managed.finish(id, &lane);
    refusal_names_fd_holder(&result, holder_pid, &lane);
    assert!(
        lane.is_dir(),
        "managed lane was deleted despite its open FD"
    );
    assert_eq!(fs::read(&sentinel).unwrap(), b"CAD848-MANAGED-KEEP\n");
    assert!(git(
        &managed.home,
        &managed.repo,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    )
    .starts_with(&git(&managed.home, &managed.repo, &["rev-parse", &branch])));
    assert!(
        holder.child.try_wait().unwrap().is_none(),
        "guard killed the holder"
    );
    drop(holder);

    // Foreign ownership: the issue's declared repo is `managed.repo`, but
    // the candidate checkout belongs to a separate repo, sits outside its
    // managed worktree layout, and has no Cadence setup/ownership record.
    // A clean merged branch makes it otherwise removable by Git evidence.
    let foreign = Fixture::new("foreign");
    let foreign_id = "C84-2";
    let foreign_repo = foreign._root.path().join("foreign-repo");
    init_repo(&foreign.home, &foreign_repo);
    let foreign_lane = foreign._root.path().join("foreign-checkout");
    let foreign_branch = "cadence/c84-2-foreign";
    git(
        &foreign.home,
        &foreign_repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            foreign_branch,
            foreign_lane.to_str().unwrap(),
            "main",
        ],
    );
    commit_and_merge(
        &foreign.home,
        &foreign_repo,
        &foreign_lane,
        foreign_branch,
        "CAD848-FOREIGN-KEEP\n",
    );
    add_refs(&foreign.pm, foreign_id, &foreign_lane, foreign_branch);
    let inventory = cadence_agent::worktree::lifecycle::inventory(&foreign_repo).unwrap();
    let foreign_lane_c = foreign_lane.canonicalize().unwrap();
    let unmanaged = inventory["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["path"]
                .as_str()
                .and_then(|path| Path::new(path).canonicalize().ok())
                .as_deref()
                == Some(foreign_lane_c.as_path())
        })
        .expect("foreign linked checkout appears in real inventory");
    assert_eq!(unmanaged["status"], "unmanaged", "{unmanaged}");
    assert!(
        unmanaged["reason_code"]
            .as_str()
            .unwrap_or_default()
            .contains("inventory-only"),
        "unknown checkout must require explicit adoption: {unmanaged}"
    );
    assert_eq!(inventory["cleanup_mode"], "report-only");
    let foreign_sentinel = foreign_lane.join("work.txt");
    assert_eq!(
        fs::read(&foreign_sentinel).unwrap(),
        b"CAD848-FOREIGN-KEEP\n"
    );
    let branch_tip = git(&foreign.home, &foreign_repo, &["rev-parse", foreign_branch]);
    let result = foreign.finish(foreign_id, &foreign_lane);
    refusal_text(
        &result,
        &[
            "unmanaged",
            "foreign",
            "ownership",
            "not managed",
            "not registered",
            "declared repo",
            "project repo",
            "repo mismatch",
        ],
    );
    assert!(foreign_lane.is_dir(), "foreign checkout was deleted");
    assert_eq!(
        fs::read(&foreign_sentinel).unwrap(),
        b"CAD848-FOREIGN-KEEP\n"
    );
    assert_eq!(
        git(&foreign.home, &foreign_repo, &["rev-parse", foreign_branch]),
        branch_tip,
        "foreign branch history was changed"
    );
    assert!(
        git(
            &foreign.home,
            &foreign_repo,
            &["worktree", "list", "--porcelain"]
        )
        .contains(foreign_lane.to_str().unwrap()),
        "foreign Git worktree registration was removed"
    );
}
