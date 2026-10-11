//! CAD-1377 independent acceptance check (reviewer-written) for the
//! bounded, targeted `issue reconcile --issue <ID>` selector.
//!
//! Real `cadence` binary against managed, merged, aged lanes in a
//! private PM/repo/state under a short `/tmp` root — the CAD-1196
//! finish fixture shape. Every `git`, `gh` and `ssh` the sweep spawns
//! is traced through a PATH shim that logs argv+cwd then exec-forwards
//! to the real binary: the trace proves which lanes were probed, not
//! a stubbed answer. Own-uid processes are started and stopped only
//! by this test. Nothing here touches the real PM, state dir, daemon
//! or network.
//!
//! * `--issue <ID> --dry-run` reports the selected merged ticket and
//!   writes nothing;
//! * `--issue <ID>` closes it and sweeps its merged lane while a
//!   coexisting UNRELATED merged lane keeps its directory, files,
//!   branch and `doing` status — and the trace shows no probe for it;
//! * `--issue <ID> --limit 1` reaches the selected ticket even when an
//!   unrelated candidate sorts first (selection is not a global prefix
//!   cap — the original CAD-1377 defect);
//! * bare `--limit 1 --dry-run` (no selector) bounds cleanup, not only
//!   classification: the unexamined second candidate is deferred
//!   before any probe, `partial` reports the run incomplete, and its
//!   lane/branch/file/status are untouched;
//! * an own-uid live process inside the selected lane defers cleanup —
//!   `done` status is NOT delete proof, the `finish::sweep_row` guard
//!   decides; released, the same run sweeps it.
//!
//! Interface confirmed against the landed CLI —
//! `scripts/acceptance-cad1377-contract.md`: argv is
//! `issue reconcile --issue <ID> [--project K] [--dry-run] [--limit N]
//! [--json]`; rows are findable by `issue` under `rows[]` and
//! `finish.rows[]`, and a guard-blocked cleanup reports `partial`.

use cadence_agent::issue::{
    parse,
    start::{self, StartArgs},
    write, Pm,
};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const TEST: &str = "cad1377_targeted_reconcile_acceptance";
const ISOLATED: &str = "CADENCE_CAD1377_ACCEPT_ISOLATED";
const ELEVATED: &str = "CADENCE_CAD1377_ACCEPT_ELEVATED";
const PROJECT: &str = "bound";
const PREFIX: &str = "C77";
const ACTOR: &str = "bound-author";

struct Fixture {
    // Held only for its Drop: keeps the fixture root alive for the
    // fixture's lifetime, cleaning the /tmp tree on drop.
    _root: TempDir,
    home: PathBuf,
    state: PathBuf,
    pm: Pm,
    repo: PathBuf,
    traced_bin: PathBuf,
    tool_log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("c1377-")
            .tempdir_in("/tmp")
            .expect("isolated fixture root");
        let home = root.path().join("h");
        let state = root.path().join("s");
        let repo = root.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        init_repo(&home, &repo);
        let mut pm = Pm::init(&root.path().join("pm")).expect("isolated PM");
        // The board view reads the notes dir — keep it under the
        // fixture root, never the default /var/www/agent-notes.
        pm.config.notes_dir = root.path().join("notes").display().to_string();
        fs::write(
            pm.dir.join("pm.yaml"),
            serde_yaml::to_string(&pm.config).expect("pm config"),
        )
        .unwrap();
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
        // PATH shim: log argv + cwd of every git/gh/ssh invocation,
        // then exec the REAL tool — the trace is evidence, not a
        // fake answer. Resolved once so the shims never recurse.
        let traced_bin = root.path().join("bin");
        fs::create_dir_all(&traced_bin).unwrap();
        let tool_log = root.path().join("tool-trace.log");
        for tool in ["git", "gh", "ssh"] {
            // Only shim tools the host actually has — an absent `gh`
            // or `ssh` must not fail the fixture (its non-use is the
            // point); a run that needs one still sees a clean
            // not-found from the real PATH.
            let Some(real) = which(tool) else { continue };
            fs::write(
                traced_bin.join(tool),
                format!(
                    "#!/bin/sh\nprintf '%s\\t%s\\n' \"$PWD\" \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
                    tool_log.display(),
                    real.display()
                ),
            )
            .unwrap();
            shim_exec(&traced_bin.join(tool));
        }
        Self {
            _root: root,
            home,
            state,
            pm,
            repo,
            traced_bin,
            tool_log,
        }
    }

    /// A `doing` issue with a real managed lane that is committed,
    /// merged into main, pushed and aged — the only thing that can
    /// stop its cleanup is a live-process guard refusal.
    fn merged_doing_lane(&self, n: u32, name: &str) -> (String, PathBuf, String, String) {
        let id = format!("{PREFIX}-{n}");
        write::new_issue(
            &self.pm,
            &self.repo,
            Some(PROJECT),
            &format!("{name} fixture"),
            Some("P2"),
            None,
            &[],
            None,
            None,
            &[],
            Some(&id),
            Some("Isolated targeted-reconcile fixture."),
            ACTOR,
        )
        .expect("fixture issue");
        let result = start::run(
            &self.pm,
            &id,
            &StartArgs {
                repo: Some(self.repo.clone()),
                name: Some(name.to_string()),
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
        // The content names the lane: the selected lane's merge
        // lands `work.txt` on main, so a later lane branching off it
        // would see an identical file and `git commit` would fail
        // with "nothing to commit". Unique bytes keep every lane's
        // commit real — and make the unrelated lane's file a content
        // sentinel for the no-touch assertions.
        let content = format!("work {name}\n");
        fs::write(lane.join("work.txt"), &content).unwrap();
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
        // `issue start` records a claim. Reconcile HOLDS a claimed
        // ticket whose merge evidence carries no timestamp
        // (`claim-undated`) — the real sweep only closes claims that
        // lapsed or were released. Re-render the file without the
        // claim through the real parse/render layer so the ticket
        // stays `doing` with its refs, like an aged merged lane
        // whose claim expired.
        let dir = self.pm.dir.join(PROJECT).join(&id);
        let file = dir.join("issue.md");
        let (mut front, body) = parse::parse_issue(&fs::read_to_string(&file).expect("issue.md"))
            .expect("parse issue.md");
        front.claim = None;
        fs::write(
            &file,
            parse::render(&front, &body).expect("render issue.md"),
        )
        .expect("claim release write");
        self.pm
            .commit(&[file], &format!("{id}: release claim (fixture)"))
            .expect("claim release commit");
        (id, lane, branch, content)
    }

    /// The trace lines spawned since `mark` (a line count).
    fn trace_since(&self, mark: usize) -> Vec<String> {
        fs::read_to_string(&self.tool_log)
            .unwrap_or_default()
            .lines()
            .skip(mark)
            .map(|l| l.to_string())
            .collect()
    }

    fn trace_len(&self) -> usize {
        fs::read_to_string(&self.tool_log)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    /// `cadence issue reconcile …` under the traced PATH and isolated
    /// HOME/XDG. `--issue <ID>` is the CAD-1377 selector this check
    /// pins.
    fn reconcile(&self, extra: &[String]) -> (Output, Vec<String>) {
        let mark = self.trace_len();
        let mut argv: Vec<String> = vec!["issue".into(), "reconcile".into()];
        argv.extend(extra.iter().cloned());
        let mut cmd = Command::new(BINARY);
        cmd.args(&argv)
            .env("CADENCE_PM_DIR", &self.pm.dir)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env(
                "PATH",
                format!("{}:{}", self.traced_bin.display(), base_path()),
            )
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_ALIAS")
            // The swept `cadence` always enumerates the real /proc:
            // in a test-seam build an ambient CADENCE_TEST_PROC_ROOT
            // would silently substitute a caller-provided proc view.
            .env_remove("CADENCE_TEST_PROC_ROOT")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR");
        let out = cadence_agent::reaper::output(&mut cmd).expect("run real issue reconcile CLI");
        let calls = self.trace_since(mark);
        (out, calls)
    }

    fn issue_status(&self, id: &str) -> String {
        let text = fs::read_to_string(self.pm.dir.join(PROJECT).join(id).join("issue.md"))
            .expect("issue.md");
        text.lines()
            .find_map(|l| l.strip_prefix("status:"))
            .map(|s| s.trim().to_string())
            .expect("status line")
    }
}

fn which(tool: &str) -> Option<PathBuf> {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", &format!("command -v {tool}")]);
    let out = cadence_agent::reaper::output(&mut cmd).expect("which");
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(unix)]
fn shim_exec(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn base_path() -> String {
    let real = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    format!("/usr/bin:/bin:{real}")
}

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "user.name=CAD-1377 bound",
        "-c",
        "user.email=bound@invalid",
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

/// The sweep row(s) for `id` in a `--json` reconcile report — both
/// the status row and any finish-sweep row carry `issue`.
fn rows_for(out: &Value, id: &str) -> Vec<Value> {
    let mut found: Vec<Value> = out["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["issue"].as_str() == Some(id))
        .cloned()
        .collect();
    found.extend(
        out["finish"]["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["issue"].as_str() == Some(id))
            .cloned(),
    );
    found
}

fn mentions(calls: &[String], needle: &str) -> bool {
    calls.iter().any(|l| l.contains(needle))
}

/// An own-uid shell that signals readiness then blocks on stdin —
/// a live process whose cwd is inside the guarded lane.
struct Holder {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Holder {
    fn start(cwd: &Path) -> Self {
        let mut command = Command::new("sh");
        command
            .args(["-c", "printf 'ready\\n'; IFS= read -r _", "cad1377-holder"])
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

/// True only on a hosted GitHub Linux runner, where passwordless
/// `sudo` is the supported way to give this check the kernel's real
/// proc visibility (CAP_SYS_PTRACE reads kthreadd's ns link and every
/// foreign cwd/fd; an unprivileged runner gets EACCES). A local or
/// non-Linux run never elevates — including on the shared host.
fn elevated_ci() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("GITHUB_ACTIONS").as_deref() == Some(std::ffi::OsStr::new("true"))
            && unsafe { libc::geteuid() } != 0
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Re-exec this exact test binary through the normal private-env
/// isolation — the unprivileged path every non-CI run takes.
fn run_isolated_child() -> Output {
    let sandbox = Builder::new()
        .prefix("c1377run-")
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
        // The re-exec and every CLI it spawns must prove the real
        // proc view — an inherited test-seam root is never valid
        // evidence for this check.
        .env_remove("CADENCE_TEST_PROC_ROOT")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cadence_agent::reaper::output(&mut cmd).unwrap()
}

/// CI-only bootstrap: re-exec this exact test binary as real root via
/// `/usr/bin/sudo -n -- /usr/bin/env -i`. No shell, no `-E`, no
/// inherited argv beyond the pinned env pairs. The scrubbed child
/// enters the ISOLATED branch and allocates every private
/// HOME/PM/state/fixture dir itself — nothing root-owned is created
/// under runner-owned temp trees. `env -i` keeps credentials, SSH
/// agent and CADENCE identity vars out; PATH is fixed to system tool
/// dirs; the only suite variable carried through is the non-secret
/// CADENCE_SUITE_LOCK path. A missing or refused `sudo -n` fails this
/// check honestly — there is no unprivileged fallback that could
/// pass. Elevation is a private-fixture context for this one check,
/// never nextest or the suite itself. Portable: only `elevated_ci()`
/// gates it to Linux.
fn run_elevated() -> Output {
    let mut cmd = Command::new("/usr/bin/sudo");
    cmd.args(["-n", "--", "/usr/bin/env", "-i"])
        .arg(format!("{ISOLATED}=1"))
        .arg(format!("{ELEVATED}=1"))
        .arg("PATH=/usr/local/bin:/usr/bin:/bin");
    if let Some(lock) = std::env::var_os("CADENCE_SUITE_LOCK") {
        cmd.arg(format!("CADENCE_SUITE_LOCK={}", lock.to_string_lossy()));
    }
    cmd.arg(std::env::current_exe().unwrap()).args([
        "--exact",
        TEST,
        "--test-threads",
        "1",
        "--nocapture",
    ]);
    cadence_agent::reaper::output(&mut cmd).expect("elevated isolated run")
}

#[test]
fn cad1377_targeted_reconcile_acceptance() {
    if std::env::var_os(ISOLATED).is_none() {
        let out = if elevated_ci() {
            run_elevated()
        } else {
            run_isolated_child()
        };
        assert!(out.status.success(), "isolated run failed: {}", text(&out));
        assert!(
            text(&out).contains("1 passed"),
            "check did not run: {}",
            text(&out)
        );
        return;
    }

    // An elevated child allocates its own private HOME/XDG root
    // first — the unprivileged parent created nothing for it, so no
    // root-owned tree is left inside runner-owned temp dirs. TempDir
    // Drop cleans it, pass or panic.
    let _elevated_root = if std::env::var_os(ELEVATED).is_some() {
        // The marker is never proof of privilege — confirm the real
        // effective uid before allocating the private root.
        #[cfg(target_os = "linux")]
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "elevation marker present without real root"
        );
        let root = Builder::new()
            .prefix("c1377elev-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", root.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", root.path().join("data"));
        std::env::set_var("XDG_STATE_HOME", root.path().join("state"));
        std::env::set_var("XDG_CACHE_HOME", root.path().join("cache"));
        std::env::remove_var(ELEVATED);
        Some(root)
    } else {
        None
    };

    // Bounded context proof in the test log: either this check sees
    // the real pid-2 ns link (elevated CI) or the unchanged
    // completeness guard fails honestly. Link targets only — no env,
    // no process contents.
    #[cfg(target_os = "linux")]
    {
        let link = |p: &str| match fs::read_link(p) {
            Ok(t) if t.as_os_str().is_empty() => "empty".to_string(),
            Ok(t) => t.display().to_string(),
            Err(e) => format!("unreadable:{e}"),
        };
        eprintln!(
            "cad1377 proc view: euid={} self/ns/pid={} 2/ns/pid={}",
            unsafe { libc::geteuid() },
            link("/proc/self/ns/pid"),
            link("/proc/2/ns/pid"),
        );
    }

    // Selected ticket gets the LOWER id — board order examines it
    // first, so the unrelated first-sorting lane is the strongest
    // "not global" witness: any unbounded sweep would classify,
    // close and clean it too.
    let fx = Fixture::new();
    let (id, lane, branch, _content) = fx.merged_doing_lane(1, "cad-77-1-selected");
    let (other_id, other_lane, other_branch, other_content) =
        fx.merged_doing_lane(2, "cad-77-2-unrelated");
    let other_sentinel = other_lane.join("work.txt");
    assert!(lane.is_dir() && other_lane.is_dir() && other_sentinel.is_file());
    // Positive real-probe evidence for the FIXTURE itself: the CLI
    // must at minimum read the lane's repo for its own evidence — a
    // reconcile that answered without touching the selected lane's
    // git would prove nothing. The unrelated lane's directory must
    // NEVER appear as a probe cwd or argument.
    let lane_s = lane.display().to_string();
    let other_s = other_lane.display().to_string();

    // 1. Targeted dry run: reports the selected ticket, writes nothing.
    let (out, calls) = fx.reconcile(&[
        "--issue".into(),
        id.clone(),
        "--project".into(),
        PROJECT.into(),
        "--dry-run".into(),
        "--json".into(),
    ]);
    assert!(
        out.status.success(),
        "targeted dry run failed: {}",
        text(&out)
    );
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("--json emitted no JSON: {}", text(&out)));
    let rows = rows_for(&report, &id);
    assert!(
        !rows.is_empty(),
        "selected {id} absent from dry run: {report}"
    );
    assert!(
        rows_for(&report, &other_id).is_empty(),
        "unrelated {other_id} examined by a targeted sweep: {report}"
    );
    assert_eq!(fx.issue_status(&id), "doing", "dry run wrote status");
    assert_eq!(
        fx.issue_status(&other_id),
        "doing",
        "dry run touched unrelated status"
    );
    assert!(
        lane.is_dir() && other_sentinel.is_file(),
        "dry run removed work"
    );
    assert!(
        mentions(&calls, &lane_s) || mentions(&calls, &branch),
        "no real probe of the selected lane in the trace: {calls:?}"
    );
    assert!(
        !mentions(&calls, &other_s),
        "unrelated lane probed from its dir: {calls:?}"
    );

    // 1b. Bare `--limit 1 --dry-run` (no --issue): the bound covers
    // cleanup, not only classification — a bounded run sweeps ONLY
    // the lanes of the candidate it actually examined. The
    // first-sorting selected ticket classifies would-close and its
    // lanes face the dry-run sweep; the second candidate is deferred
    // BEFORE its probe. An unconditional global cleanup behind a
    // bounded classifier would report the other ticket done and
    // probe its lanes — this fails on that branch, not on the
    // selector.
    let (out, calls) = fx.reconcile(&[
        "--project".into(),
        PROJECT.into(),
        "--limit".into(),
        "1".into(),
        "--dry-run".into(),
        "--json".into(),
    ]);
    assert!(
        out.status.success(),
        "bounded dry run failed: {}",
        text(&out)
    );
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("--json emitted no JSON: {}", text(&out)));
    assert!(
        !rows_for(&report, &id).is_empty(),
        "first-sorting {id} absent from the bounded run: {report}"
    );
    assert!(
        rows_for(&report, &other_id).is_empty(),
        "uncapped candidate {other_id} got a row: {report}"
    );
    assert!(
        report["deferred"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| v.as_str() == Some(&other_id))
            || report["cleanup_deferred"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|v| v.as_str() == Some(&other_id)),
        "uncapped {other_id} neither deferred nor cleanup-deferred: {report}"
    );
    assert_eq!(
        report["partial"],
        Value::Bool(true),
        "a bounded run that left a candidate unexamined reported complete: {report}"
    );
    // The dry run wrote nothing and nothing was swept — either lane.
    assert_eq!(
        fx.issue_status(&id),
        "doing",
        "bounded dry run wrote status"
    );
    assert_eq!(
        fx.issue_status(&other_id),
        "doing",
        "bounded dry run touched the uncapped ticket"
    );
    assert!(
        lane.is_dir() && other_lane.is_dir() && other_sentinel.is_file(),
        "bounded dry run removed work"
    );
    assert_eq!(
        fs::read_to_string(&other_sentinel).unwrap(),
        other_content,
        "bounded dry run rewrote the uncapped lane's file"
    );
    assert!(
        !git(&fx.home, &fx.repo, &["branch", "--list", &other_branch]).is_empty(),
        "bounded dry run deleted the uncapped branch"
    );
    // The examined ticket's lane WAS probed (positive evidence the
    // sweep ran for real); the deferred candidate's lane and branch
    // never appear in the trace — deferral is decided from refs
    // before any probe.
    assert!(
        mentions(&calls, &lane_s) || mentions(&calls, &branch),
        "no real probe of the examined lane under --limit 1: {calls:?}"
    );
    assert!(
        !mentions(&calls, &other_s) && !mentions(&calls, &other_branch),
        "deferred candidate probed by the bounded run: {calls:?}"
    );

    // 2. Targeted real run: the selected ticket reconciles — status
    // done, merged lane swept — the unrelated lane is untouched in
    // directory, files, branch ref and status. A `finished` sweep row
    // is required: a refused or missing row can never read as a swept
    // lane, and the full report is the failure evidence.
    let (out, calls) = fx.reconcile(&["--issue".into(), id.clone(), "--json".into()]);
    assert!(
        out.status.success(),
        "targeted reconcile failed: {}",
        text(&out)
    );
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("--json emitted no JSON: {}", text(&out)));
    assert_eq!(
        fx.issue_status(&id),
        "done",
        "selected ticket not reconciled: {report}"
    );
    let finish_rows = rows_for(&report, &id);
    assert!(
        finish_rows
            .iter()
            .any(|r| r["outcome"].as_str() == Some("finished")),
        "selected lane's sweep row did not finish: {report}"
    );
    assert!(
        report["finish_refused"].as_u64() == Some(0) && report["partial"] == Value::Bool(false),
        "targeted sweep of a proven-idle lane refused or reported partial: {report}"
    );
    assert!(
        !lane.exists(),
        "selected merged lane survived the targeted sweep: {report}"
    );
    assert!(
        git(&fx.home, &fx.repo, &["branch", "--list", &branch]).is_empty(),
        "selected lane branch survived the targeted sweep: {report}"
    );
    assert_eq!(
        fx.issue_status(&other_id),
        "doing",
        "a targeted sweep closed the unrelated ticket"
    );
    assert!(other_lane.is_dir(), "unrelated lane directory removed");
    assert!(other_sentinel.is_file(), "unrelated lane work deleted");
    assert_eq!(
        fs::read_to_string(&other_sentinel).unwrap(),
        other_content,
        "unrelated lane file content changed"
    );
    assert!(
        !git(&fx.home, &fx.repo, &["branch", "--list", &other_branch]).is_empty(),
        "unrelated lane branch deleted"
    );
    assert!(
        !mentions(&calls, &other_s) && !mentions(&calls, &other_branch),
        "unrelated lane probed (git/gh/ssh argv or cwd) by the targeted run: {calls:?}"
    );

    // 3. A bound must not cap selection globally: a fresh fixture
    // where the UNRELATED candidate sorts first (C77-1) still reaches
    // the selected later id under --limit 1.
    let fx2 = Fixture::new();
    let (_other2_id, other2_lane, other2_branch, _c1) =
        fx2.merged_doing_lane(1, "cad-77-1-unrelated");
    let (sel2, lane2, _branch2, _c2) = fx2.merged_doing_lane(2, "cad-77-2-selected");
    let (out, calls) = fx2.reconcile(&[
        "--issue".into(),
        sel2.clone(),
        "--limit".into(),
        "1".into(),
        "--json".into(),
    ]);
    assert!(
        out.status.success(),
        "targeted --limit 1 reconcile failed: {}",
        text(&out)
    );
    assert_eq!(
        fx2.issue_status(&sel2),
        "done",
        "--limit 1 starved the selected ticket behind a first-sorting candidate"
    );
    assert!(!lane2.exists(), "selected lane not swept under --limit 1");
    assert_eq!(
        fx2.issue_status(&_other2_id),
        "doing",
        "a bounded targeted sweep reached past its ticket"
    );
    assert!(other2_lane.is_dir(), "first-sorting unrelated lane removed");
    assert!(
        !git(&fx2.home, &fx2.repo, &["branch", "--list", &other2_branch]).is_empty(),
        "first-sorting unrelated branch deleted"
    );
    let other2_s = other2_lane.display().to_string();
    assert!(
        !mentions(&calls, &other2_s) && !mentions(&calls, &other2_branch),
        "unrelated first-sorting lane probed by the bounded run: {calls:?}"
    );

    // 4. Live-process guard: with this run's own process holding its
    // cwd inside the selected lane, cleanup defers — status may close
    // on merge evidence, but `done` is NOT delete proof: the worktree
    // and its branch survive.
    let fx3 = Fixture::new();
    let (id3, lane3, branch3, _c3) = fx3.merged_doing_lane(1, "cad-77-1-guarded");
    let holder = Holder::start(&lane3);
    let (out, _calls) = fx3.reconcile(&["--issue".into(), id3.clone(), "--json".into()]);
    // The close may land and the sweep row must defer or refuse —
    // either way the run answers, never errors the whole CLI.
    assert!(
        out.status.success() || text(&out).contains("refused"),
        "guarded reconcile crashed: {}",
        text(&out)
    );
    assert!(
        lane3.is_dir(),
        "worktree deleted while a live own-uid process stands inside it"
    );
    assert!(
        !git(&fx3.home, &fx3.repo, &["branch", "--list", &branch3]).is_empty(),
        "branch deleted while a live own-uid process stands in the lane"
    );
    // The blocked cleanup must surface as `partial` — a held sweep
    // that reports complete is the false-green this check exists to
    // catch.
    if out.status.success() {
        let report: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("--json emitted no JSON: {}", text(&out)));
        assert_eq!(
            report["partial"],
            Value::Bool(true),
            "guarded cleanup blocked by a live holder reported complete: {report}"
        );
        assert!(
            rows_for(&report, &id3)
                .iter()
                .any(|r| r["outcome"].as_str() == Some("refused")
                    && r["reason"]
                        .as_str()
                        .is_some_and(|why| why.contains("cwd inside"))),
            "the held lane's row does not name the live process: {report}"
        );
    }
    drop(holder);

    // 5. Once the holder is gone the same targeted verb completes:
    // the guard above — not an unrelated condition — was the sole
    // deferral.
    let (out, _calls) = fx3.reconcile(&["--issue".into(), id3.clone(), "--json".into()]);
    assert!(
        out.status.success(),
        "re-run after release failed: {}",
        text(&out)
    );
    assert!(
        !lane3.exists(),
        "released lane not swept on the follow-up run: {}",
        text(&out)
    );
    let _ = std::io::stdout().flush();
}
