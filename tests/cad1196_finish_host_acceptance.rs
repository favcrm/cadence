//! CAD-1196 independent acceptance check (reviewer-written) for the
//! `issue finish` process-enumeration gate on a normal non-root host.
//!
//! Real `cadence issue finish` against a managed, merged, aged lane in a
//! private PM/repo/state under a short `/tmp` root. Own-uid processes are
//! started by the test and killed by it. Nothing here touches the real PM,
//! state dir or daemon.
//!
//! * clean lane finishes (the host-specific kernel behaviour: empty PID 2
//!   ns link, EACCES on other users' processes must not block a non-root
//!   operator);
//! * an own-uid process whose cwd is inside the lane refuses, naming it;
//! * an own-uid process holding an fd inside (cwd elsewhere) refuses;

use cadence_agent::issue::{
    start::{self, StartArgs},
    write, Pm,
};
use std::fs;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const TEST: &str = "cad1196_finish_enumeration_on_a_normal_host";
const ISOLATED: &str = "CADENCE_CAD1196_ACCEPT_ISOLATED";
const PROJECT: &str = "guard";
const PREFIX: &str = "C96";
const ACTOR: &str = "guard-author";

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    state: PathBuf,
    pm: Pm,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("c1196-")
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
        write::new_issue(
            &pm,
            &repo,
            Some(PROJECT),
            "finish fixture",
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some(&format!("{PREFIX}-1")),
            Some("Isolated finish enumeration fixture."),
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

    /// A merged, clean, aged managed lane: the ONLY thing that can stop
    /// `finish` is live process use.
    fn merged_lane(&self) -> (String, PathBuf, String) {
        let id = format!("{PREFIX}-1");
        let result = start::run(
            &self.pm,
            &id,
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
        let lane = PathBuf::from(result["worktree"].as_str().unwrap());
        let branch = result["branch"].as_str().unwrap().to_string();
        fs::write(lane.join("work.txt"), "work\n").unwrap();
        git(&self.home, &lane, &["add", "work.txt"]);
        git(
            &self.home,
            &lane,
            &["commit", "--quiet", "-m", "fixture work"],
        );
        git(
            &self.home,
            &self.repo,
            &["merge", "--quiet", "--ff-only", &branch],
        );
        git(
            &self.home,
            &self.repo,
            &["push", "--quiet", "origin", "main"],
        );
        git(&self.home, &self.repo, &["fetch", "--quiet", "origin"]);
        age(&lane);
        (id, lane, branch)
    }

    fn finish(&self, id: &str, lane: &Path, force: bool) -> Output {
        let mut cmd = Command::new(BINARY);
        cmd.args(["issue", "finish", id, "--worktree", lane.to_str().unwrap()]);
        if force {
            cmd.arg("--force");
        }
        cmd.env("CADENCE_PM_DIR", &self.pm.dir)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_ALIAS")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR");
        cadence_agent::reaper::output(&mut cmd).expect("run real issue finish CLI")
    }
}

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=CAD-1196 guard",
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
        "git {} failed: {}{}",
        args.join(" "),
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

fn age(lane: &Path) {
    let then = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(2 * 60 * 60);
    for name in [".gitignore", "tracked.txt", "work.txt"] {
        let mut cmd = Command::new("touch");
        cmd.args(["-h", "-d", &format!("@{then}")])
            .arg(lane.join(name));
        let out = cadence_agent::reaper::output(&mut cmd).expect("age fixture file");
        assert!(out.status.success(), "touch failed: {name}");
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// An own-uid shell that signals readiness then blocks on stdin.
struct Holder {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Holder {
    /// `script` runs in `cwd`; it must print `ready` once set up.
    fn start(script: &str, cwd: &Path, arg: &Path) -> Self {
        let mut command = Command::new("sh");
        command
            .args(["-c", script, "cad1196-holder", arg.to_str().unwrap()])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cadence_agent::reaper::spawn(&mut command).expect("start holder");
        let stdin = child.stdin.take().unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .expect("holder readiness");
        assert_eq!(ready.trim(), "ready", "holder failed to start");
        Self {
            child,
            stdin: Some(stdin),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn refused_naming(out: &Output, pid: u32, what: &str, lane: &Path) {
    let t = text(out);
    assert!(
        !out.status.success(),
        "finish succeeded despite holder {pid}: {t}"
    );
    let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
        .unwrap()
        .trim()
        .to_string();
    let finding = format!("process {pid} ({comm}) {what} {}", lane.display()).to_ascii_lowercase();
    assert!(
        t.to_ascii_lowercase().contains(&finding),
        "refusal did not attribute {what} to PID {pid}; expected {finding:?}: {t}"
    );
}

#[test]
fn cad1196_finish_enumeration_on_a_normal_host() {
    if std::env::var_os(ISOLATED).is_none() {
        let sandbox = Builder::new()
            .prefix("c1196run-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = sandbox.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
            .env(ISOLATED, "1")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", sandbox.path().join("config"))
            .env("XDG_DATA_HOME", sandbox.path().join("data"))
            .env("XDG_STATE_HOME", sandbox.path().join("state"))
            .env("XDG_CACHE_HOME", sandbox.path().join("cache"))
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_ALIAS")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        let out = cadence_agent::reaper::output(&mut cmd).unwrap();
        assert!(out.status.success(), "isolated run failed: {}", text(&out));
        assert!(
            text(&out).contains("1 passed"),
            "check did not run: {}",
            text(&out)
        );
        return;
    }

    // 1. A clean lane. The kernel behaviour the ticket covers is an EMPTY
    // `2/ns/pid` link for a non-root reader (Linux 7.0 on this host): there
    // the lane must finish. Kernels that answer EACCES instead (the CI
    // runner) cannot be proven complete and must still fail closed, with
    // that reason, and leave the lane untouched.
    let pid2_link_is_empty = matches!(
        fs::read_link("/proc/2/ns/pid"),
        Ok(target) if target.as_os_str().is_empty()
    );
    let clean = Fixture::new();
    let (id, lane, branch) = clean.merged_lane();
    let out = clean.finish(&id, &lane, false);
    if pid2_link_is_empty {
        assert!(
            out.status.success(),
            "clean lane must finish: {}",
            text(&out)
        );
        assert!(!lane.exists(), "clean lane was not removed");
        assert!(
            git(&clean.home, &clean.repo, &["branch", "--list", &branch]).is_empty(),
            "branch not removed"
        );
    } else {
        assert!(!out.status.success(), "{}", text(&out));
        assert!(text(&out).contains("2/ns/pid"), "{}", text(&out));
        assert!(lane.is_dir(), "fail-closed refusal removed the lane");
    }

    // 2. Own-uid process with its cwd inside refuses.
    let cwd_case = Fixture::new();
    let (id, lane, branch) = cwd_case.merged_lane();
    let cwd_holder = Holder::start("printf 'ready\\n'; IFS= read -r _", &lane, &lane);
    let out = cwd_case.finish(&id, &lane, false);
    refused_naming(&out, cwd_holder.pid(), "has cwd inside", &lane);
    assert!(lane.is_dir(), "lane deleted despite cwd holder");
    assert!(
        !git(
            &cwd_case.home,
            &cwd_case.repo,
            &["branch", "--list", &branch]
        )
        .is_empty(),
        "branch deleted despite cwd holder"
    );
    drop(cwd_holder);

    // 3. Own-uid process holding an fd inside, cwd elsewhere, refuses.
    let fd_case = Fixture::new();
    let (id, lane, _branch) = fd_case.merged_lane();
    let elsewhere = fd_case._root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let sentinel = lane.join("work.txt");
    let fd_holder = Holder::start(
        "exec 9<\"$1\"; printf 'ready\\n'; IFS= read -r _",
        &elsewhere,
        &sentinel,
    );
    let out = fd_case.finish(&id, &lane, false);
    refused_naming(
        &out,
        fd_holder.pid(),
        "holds an open file descriptor inside",
        &lane,
    );
    assert!(lane.is_dir(), "lane deleted despite fd holder");
    drop(fd_holder);

    // Holders gone: where the scan can complete, the same lane now finishes,
    // so the guard (not some unrelated condition) was the sole refusal above.
    if pid2_link_is_empty {
        let out = fd_case.finish(&id, &lane, false);
        assert!(
            out.status.success(),
            "lane must finish once released: {}",
            text(&out)
        );
    }
    let _ = std::io::stdout().flush();
}
