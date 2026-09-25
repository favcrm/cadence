//! tracker_issue: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use serde_json::json;
use serde_json::Value;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

#[test]
fn skill_install_links_and_is_idempotent() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();

    let out = cadence_at(home.path(), state.path(), &["skill", "install"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let file = home.path().join(".agents/skills/cadence/SKILL.md");
    assert_eq!(v["installed"].as_str().unwrap(), file.to_str().unwrap());
    assert_eq!(v["linked"].as_array().unwrap().len(), 3);
    assert!(v["skipped"].as_array().unwrap().is_empty());
    // The installed file is byte-identical to the vendored copy.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        cadence_agent::skill::SKILL_MD
    );
    for parent in [".claude/skills", ".cursor/skills", ".copilot/skills"] {
        let link = home.path().join(parent).join("cadence");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            home.path().join(".agents/skills/cadence"),
            "{parent}"
        );
    }

    // Second run: same result, links re-verified not duplicated.
    let out = cadence_at(home.path(), state.path(), &["skill", "install"]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["linked"].as_array().unwrap().len(), 0, "{v}");
    let status: Value =
        serde_json::from_slice(&cadence_at(home.path(), state.path(), &["skill", "status"]).stdout)
            .unwrap();
    assert_eq!(status["installed"], true);
    assert_eq!(status["content_match"], true);
    assert_eq!(status["links"]["claude"], "ok");
    assert_eq!(status["links"]["cursor"], "ok");
    assert_eq!(status["links"]["copilot"], "ok");
}

#[test]
fn skill_install_never_clobbers_real_entries() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // A real directory sitting where the claude symlink would go.
    let foreign = home.path().join(".claude/skills/cadence");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("KEEP"), "mine").unwrap();

    let out = cadence_at(home.path(), state.path(), &["skill", "install"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["linked"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["skipped"].as_array().unwrap().len(), 1);
    // Untouched — still a real dir with its contents.
    assert!(foreign.is_dir() && !foreign.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_to_string(foreign.join("KEEP")).unwrap(),
        "mine"
    );
    let status: Value =
        serde_json::from_slice(&cadence_at(home.path(), state.path(), &["skill", "status"]).stdout)
            .unwrap();
    assert_eq!(status["links"]["claude"], "foreign");
}

#[test]
fn skill_install_overwrites_stale_content() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    cadence_at(home.path(), state.path(), &["skill", "install"]);
    let file = home.path().join(".agents/skills/cadence/SKILL.md");
    std::fs::write(&file, "STALE").unwrap();
    let status: Value =
        serde_json::from_slice(&cadence_at(home.path(), state.path(), &["skill", "status"]).stdout)
            .unwrap();
    assert_eq!(status["content_match"], false);

    cadence_at(home.path(), state.path(), &["skill", "install"]);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        cadence_agent::skill::SKILL_MD
    );
}

#[test]
fn daemon_run_refreshes_skill_on_start() {
    let home = TempDir::new().unwrap();
    // Bound, not a temporary: the parent must outlive the daemon.
    let state_root = TempDir::new().unwrap();
    let state = state_root.path().join("state");
    let _reaper = DaemonReaper::new(&state);
    // Seed a stale copy so the refresh (not just install) is exercised.
    let file = home.path().join(".agents/skills/cadence/SKILL.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "STALE").unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["daemon", "run"])
        .env("HOME", home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while std::fs::read_to_string(&file)
        .map(|s| s.as_str() == "STALE")
        .unwrap_or(true)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        cadence_agent::skill::SKILL_MD,
        "daemon start must refresh stale skill content"
    );
    // Missing links get created too.
    assert!(home
        .path()
        .join(".claude/skills/cadence")
        .symlink_metadata()
        .is_ok());
    // Cleanly stop the daemon we spawned. The skill file lands before
    // `serve()` binds the socket, so the first `daemon stop` can race
    // the listener — retry briefly, and bound the exit wait so a wedged
    // daemon fails the test instead of hanging it.
    let stop_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let out = cadence_at(home.path(), &state, &["daemon", "stop"]);
        if out.status.success() {
            break;
        }
        assert!(
            Instant::now() < stop_deadline,
            "daemon stop never succeeded: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let wait_deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < wait_deadline,
            "daemon run never exited after daemon stop"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// CAD-43 `--job`: `issue start --job --pm --spec` opens an M3 job +
/// a worktree-scoped task through the same `job_new`/`task_new` RPCs —
/// and refuses before creating anything when the daemon can't answer.
#[test]
fn issue_start_job_opens_scoped_task() {
    let d = TestDaemon::start();
    let tmp = TempDir::new().unwrap();
    let (pm, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = git_ok();
    git_f_repo(&repo, &git, |_| {});
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    // The tracker's pre-commit hook runs `cadence` from PATH — the
    // just-built binary must come first.
    let cli = |state: &Path, args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&d.state, &["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(
        cli(
            &d.state,
            &["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]
        )
        .0
    );
    assert!(
        cli(
            &d.state,
            &["issue", "new", "Job Start", "--project", "demo"]
        )
        .0
    );
    let (spec, _sha) = d.spec_file("spec.md", "do the seeded work");
    let tracker_commits = || {
        String::from_utf8_lossy(
            &std::process::Command::new("git")
                .arg("-C")
                .arg(&pm)
                .args(["rev-list", "--count", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .parse::<usize>()
        .unwrap()
    };
    let wt = repo.join(".cadence/wt/d-1-job-start");

    // Daemon down (state dir without a socket): refused, nothing created.
    let dead = TempDir::new().unwrap();
    let (ok, err) = cli(
        dead.path(),
        &[
            "issue", "start", "D-1", "--job", "--pm", "pm", "--spec", &spec,
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("not reachable"),
        "{err}"
    );
    assert!(!wt.exists());
    let before = tracker_commits();

    // Daemon up but the pm alias unknown: also refused before creating.
    let (ok, _) = cli(
        &d.state,
        &[
            "issue", "start", "D-1", "--job", "--pm", "pm", "--spec", &spec,
        ],
    );
    assert!(!ok);
    assert!(!wt.exists());
    assert_eq!(tracker_commits(), before);

    // Daemon up, pm known, assignee unknown: refused before creating.
    d.register("pm");
    let (ok, err) = cli(
        &d.state,
        &[
            "issue",
            "start",
            "D-1",
            "--job",
            "--pm",
            "pm",
            "--spec",
            &spec,
            "--assignee",
            "ghost",
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("ghost"),
        "{err}"
    );
    assert!(!wt.exists());
    assert_eq!(tracker_commits(), before);

    // Assignee exists but outside the pm's group: refused too.
    d.register("outsider");
    let (ok, err) = cli(
        &d.state,
        &[
            "issue",
            "start",
            "D-1",
            "--job",
            "--pm",
            "pm",
            "--spec",
            &spec,
            "--assignee",
            "outsider",
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("group"),
        "{err}"
    );
    assert!(!wt.exists());
    assert_eq!(tracker_commits(), before);

    // Real path: one job, one task — <job>-t1 scoped to the worktree.
    d.register_member("w1", "pm");
    let (ok, out) = cli(
        &d.state,
        &[
            "issue",
            "start",
            "D-1",
            "--job",
            "--pm",
            "pm",
            "--spec",
            &spec,
            "--assignee",
            "w1",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["created"], true);
    assert_eq!(tracker_commits(), before + 1);
    let (job_id, task_id) = (
        out["job"].as_str().unwrap().to_string(),
        out["task"].as_str().unwrap().to_string(),
    );
    assert_eq!(task_id, format!("{job_id}-t1"));
    let show = d.rpc("job_show", json!({"job": job_id})).unwrap();
    let job = &show["job"];
    assert_eq!(job["issue"], "D-1");
    assert_eq!(job["repo"].as_str().unwrap(), repo_s);
    assert_eq!(
        job["base_ref"].as_str().unwrap(),
        out["base"]["sha"].as_str().unwrap()
    );
    let tasks = job["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "{job}");
    let scoped = &tasks[0];
    assert_eq!(scoped["id"].as_str().unwrap(), task_id);
    assert_eq!(scoped["worktree"].as_str().unwrap(), "d-1-job-start");
    assert_eq!(scoped["branch"].as_str().unwrap(), "cadence/d-1-job-start");
    assert_eq!(
        scoped["base_sha"].as_str().unwrap(),
        out["base"]["sha"].as_str().unwrap()
    );
    assert_eq!(scoped["assignee"].as_str().unwrap(), "w1");
}

/// `doctor --host --json` on the real host: one object, the named
/// checks, each ok|warn|fail, exit code the worst level. What the host
/// measures is its own business — this only proves the surface runs
/// and reports honestly, never which level comes back.
#[test]
fn doctor_host_json_reports_all_checks() {
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    let cwd = dir.path().join("nowhere");
    for d in [&state, &home, &cwd] {
        std::fs::create_dir_all(d).unwrap();
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["doctor", "--host", "--json"])
        .env("HOME", &home)
        .env("CADENCE_PM_DIR", home.join("pm"))
        .current_dir(&cwd)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("doctor --host --json printed no JSON: {out:?}"));
    let names: Vec<&str> = report["checks"]
        .as_array()
        .expect("checks[]")
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "disk",
            "provider-state",
            "pipes",
            "memory",
            "processes",
            "sessions",
            "pane-identity",
            "orphans",
            "temp-dirs",
            "task-targets",
            "worktrees",
            "load",
            "config",
            "tailnet",
            "agent-uid"
        ]
    );
    for c in report["checks"].as_array().unwrap() {
        assert!(matches!(c["level"].as_str(), Some("ok" | "warn" | "fail")));
        for k in ["value", "threshold", "detail", "remedy"] {
            assert!(c.get(k).is_some(), "check missing {k}: {c}");
        }
    }
    let worst = report["level"].as_str().unwrap();
    let expect = match worst {
        "fail" => 2,
        "warn" => 1,
        _ => 0,
    };
    assert_eq!(code, expect, "level {worst} should exit {expect}");
}

/// The default: `issue start` links the worktree's hashed-content
/// cargo subdirs into `<repo>/.cadence/target/shared/debug` while
/// keeping the lane's own `target/debug` real — uplifted binaries are
/// per-lane. The effective dir is recorded on the worktree ref and
/// the tree stays clean for `git status`.
#[test]
fn issue_start_links_shared_cargo_deps() {
    let s = SharedTarget::new();
    s.new_issue("Shared");
    let (ok, out) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = s.worktree_of("D-1");
    let shared_debug = s.repo.join(".cadence/target/shared/debug");
    for name in ["deps", ".fingerprint", "build", "incremental"] {
        let link = wt.join("target/debug").join(name);
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            shared_debug.join(name),
            "{name}"
        );
    }
    // `examples` is NOT shared — cargo uplifts example binaries to
    // `debug/examples/<name>` unhashed, so a shared dir would hand one
    // lane another lane's example.
    assert!(!wt.join("target/debug/examples").is_symlink());
    for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
        let link = wt.join("target/debug").join(name);
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            shared_debug.join(name),
            "{name}"
        );
    }
    // `debug/` itself is real — no `.cargo/` is written anywhere.
    assert!(!wt.join("target/debug").is_symlink());
    assert!(!wt.join(".cargo").exists());
    assert_eq!(
        out["target_dir"].as_str().unwrap(),
        wt.join("target").to_string_lossy(),
        "{out}"
    );
    assert_eq!(s.git(&wt, &["status", "--porcelain"]), "");
    // The worktree ref records the effective target dir.
    let show = s.cli(&["issue", "show", "D-1", "--json"]).1;
    let wt_ref = show["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "worktree")
        .unwrap();
    assert_eq!(
        wt_ref["cargo_target"].as_str().unwrap(),
        wt.join("target").to_string_lossy()
    );
    assert!(show["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "branch")
        .unwrap()["cargo_target"]
        .is_null());
    // Re-start is idempotent — same farm, same ref, no second commit.
    let (ok, out) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok && out["created"] == false, "{out}");
    assert_eq!(s.git(&wt, &["status", "--porcelain"]), "");
    // Re-attach: remove the dir, keep refs — start re-plants the farm.
    s.git(
        &s.repo,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    let (ok, out) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    assert!(wt.join("target/debug/deps").is_symlink());
}

/// Two lanes sharing the dep cache never share the uplifted binary:
/// each lane's `target/debug/marker` is its own file, so a lane's
/// `cargo test` execs its own code. This is the CAD-95 r2 acceptance
/// case — a shared `build.target-dir` would hand lane A lane B's
/// binary.
#[test]
fn lanes_share_deps_but_not_the_uplifted_binary() {
    let s = SharedTarget::new();
    s.new_issue("LaneA");
    s.new_issue("LaneB");
    assert!(s.cli(&["issue", "start", "D-1"]).0);
    assert!(s.cli(&["issue", "start", "D-2"]).0);
    let (wt_a, wt_b) = (s.worktree_of("D-1"), s.worktree_of("D-2"));
    // Both lanes' hashed subdirs point into the one shared cache.
    let shared_debug = s.repo.join(".cadence/target/shared/debug");
    for wt in [&wt_a, &wt_b] {
        assert_eq!(
            std::fs::read_link(wt.join("target/debug/deps")).unwrap(),
            shared_debug.join("deps")
        );
    }
    // Lane A builds "lane-A"; lane B overwrites nothing of A's when
    // it builds "lane-B".
    s.set_marker(&wt_a, "lane-A");
    s.cargo_build(&wt_a);
    s.set_marker(&wt_b, "lane-B");
    s.cargo_build(&wt_b);
    for (wt, want) in [(&wt_a, "lane-A"), (&wt_b, "lane-B")] {
        let bin = wt.join("target/debug/marker");
        let out = std::process::Command::new(&bin).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), want);
    }
    // Dep artifacts landed in the shared cache through the links.
    let deps: Vec<String> = std::fs::read_dir(shared_debug.join("deps"))
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    assert!(deps.iter().any(|d| d.starts_with("marker-")), "{deps:?}");
}

/// A tracked `.cargo/config.toml` — the file a project ships — is
/// never written, reserialized or excluded by `issue start`, and the
/// clean tree it leaves is exactly what `issue finish` checks.
#[test]
fn issue_start_never_touches_tracked_cargo_config() {
    let s = SharedTarget::new();
    // A tracked config with real settings — target-dir is absent, so
    // the effective dir stays `<wt>/target` and the farm plants.
    let conf = "[build]\njobs = 2\n\n[target.'cfg(target_os = \"linux\")']\nrustflags = [\"-C\", \"link-arg=-Wl,-rpath,/x\"]\n";
    std::fs::create_dir_all(s.repo.join(".cargo")).unwrap();
    std::fs::write(s.repo.join(".cargo/config.toml"), conf).unwrap();
    s.git(&s.repo, &["add", "-A"]);
    s.git(&s.repo, &["commit", "-qm", "cargo config"]);
    s.new_issue("Cfg");
    let (ok, _) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok);
    let wt = s.worktree_of("D-1");
    assert_eq!(
        std::fs::read_to_string(wt.join(".cargo/config.toml")).unwrap(),
        conf,
        "tracked config rewritten"
    );
    assert!(wt.join("target/debug/deps").is_symlink());
    assert_eq!(s.git(&wt, &["status", "--porcelain"]), "");
    // And a lane whose tree is genuinely clean must not be refused as
    // dirty by finish's guard — the pre-r2 rewrite left it dirty.
    let (ok, _) = s.cli(&["issue", "set", "D-1", "owner="]);
    assert!(ok);
    std::fs::write(wt.join("work.txt"), "x").unwrap();
    s.git(&wt, &["add", "-A"]);
    s.git(&wt, &["commit", "-qm", "work"]);
    idle(&wt);
    s.git(&s.repo, &["merge", "-q", "cadence/d-1-cfg"]);
    let (ok, out) = s.cli(&["issue", "finish", "D-1"]);
    assert!(ok && out["finished"] == true, "{out}");
}

/// `[build] target_dir = "per-worktree"` in project.yaml opts a lane
/// back onto its own `target/`; anything else is a rejected config.
#[test]
fn issue_start_per_worktree_and_invalid_target_dir() {
    let s = SharedTarget::new();
    s.new_issue("Lane");
    s.new_issue("Bad");
    s.set_build_target_dir("per-worktree");
    let (ok, out) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = s.worktree_of("D-1");
    assert_eq!(
        out["target_dir"].as_str().unwrap(),
        wt.join("target").to_string_lossy()
    );
    // No farm, no config — the lane is fully private.
    assert!(!wt.join("target").exists());
    assert!(!wt.join(".cargo").exists());

    s.set_build_target_dir("bogus");
    let (ok, err) = s.cli(&["issue", "start", "D-2"]);
    assert!(!ok, "{err}");
    assert!(err["error"].as_str().unwrap().contains("bogus"), "{err}");
}

/// `issue finish` removes the worktree but never the shared cache —
/// the ref's `cargo_target` is reported with a literal `exists` check.
#[test]
fn issue_finish_keeps_shared_cargo_target() {
    let s = SharedTarget::new();
    s.new_issue("Done");
    let shared = s.repo.join(".cadence/target/shared");
    let (ok, _) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok);
    std::fs::write(shared.join("dep.rlib"), "cached").unwrap();
    // Ownerless — no daemon in this fixture, and finish's owner check
    // only runs when an owner is recorded.
    let (ok, _) = s.cli(&["issue", "set", "D-1", "owner="]);
    assert!(ok);
    let wt = s.worktree_of("D-1");
    std::fs::write(wt.join("work.txt"), "x").unwrap();
    s.git(&wt, &["add", "-A"]);
    s.git(&wt, &["commit", "-qm", "work"]);
    idle(&wt);
    s.git(&s.repo, &["merge", "-q", "cadence/d-1-done"]);
    let (ok, out) = s.cli(&["issue", "finish", "D-1"]);
    assert!(ok && out["finished"] == true, "{out}");
    // The lane's own target/ went with it — `cargo_target_exists` is
    // the literal post-removal check, and the shared cache the lane
    // linked into is still there (rm unlinks, never follows).
    assert_eq!(
        out["cargo_target"].as_str().unwrap(),
        wt.join("target").to_string_lossy()
    );
    assert_eq!(out["cargo_target_exists"], false, "{out}");
    assert!(shared.join("dep.rlib").is_file());
    assert!(!wt.exists());
}

/// `doctor --host` counts the shared cache once at repo level, and
/// `--reclaim-plan` lists candidates — stale lanes, per-lane targets,
/// the shared cache — without deleting anything.
#[test]
fn doctor_host_shared_target_and_reclaim_plan() {
    let s = SharedTarget::new();
    s.new_issue("One");
    s.new_issue("Two");
    let (ok, _) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok);
    let (ok, _) = s.cli(&["issue", "start", "D-2"]);
    assert!(ok);
    let shared = s.repo.join(".cadence/target/shared");
    std::fs::create_dir_all(shared.join("debug")).unwrap();
    std::fs::write(shared.join("debug/dep.rlib"), vec![0_u8; 4096]).unwrap();
    // A per-lane target dir on D-1's worktree — the pre-CAD-95 layout.
    // A commit past base keeps the lane live: a stale lane's whole
    // dir is freed by its own row and emits no informational
    // worktree-target row.
    let wt1 = s.worktree_of("D-1");
    std::fs::write(wt1.join("wip.txt"), "x").unwrap();
    s.git(&wt1, &["add", "-A"]);
    s.git(
        &wt1,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "wip",
        ],
    );
    std::fs::create_dir_all(wt1.join("target/debug")).unwrap();
    std::fs::write(wt1.join("target/debug/dep.rlib"), vec![0_u8; 2048]).unwrap();

    let (_code, stdout, _) = s.cli_at(&s.repo, &["doctor", "--host", "--json"]);
    let report: Value = serde_json::from_str(stdout.trim()).unwrap();
    let wt_check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "worktrees")
        .unwrap();
    assert_eq!(
        wt_check["value"]["shared_cargo_target"]["path"]
            .as_str()
            .unwrap(),
        shared.to_string_lossy(),
        "{wt_check}"
    );
    assert!(
        wt_check["value"]["shared_cargo_target"]["bytes"]
            .as_u64()
            .unwrap()
            >= 4096
    );

    let (code, stdout, _) = s.cli_at(&s.repo, &["doctor", "--host", "--reclaim-plan", "--json"]);
    // The plan is the full report plus a `reclaim` section — the exit
    // code is the worst check level, matching the plain report's.
    let expected = match report["level"].as_str().unwrap() {
        "fail" => 2,
        "warn" => 1,
        _ => 0,
    };
    assert_eq!(code, expected, "{stdout}");
    let merged: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        merged["checks"].as_array().unwrap().len() == report["checks"].as_array().unwrap().len()
    );
    let plan = &merged["reclaim"];
    let kinds: Vec<&str> = plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"worktree-target") && kinds.contains(&"shared-cargo-cache"),
        "{kinds:?}"
    );
    // Nothing deleted — both dirs still present.
    assert!(wt1.join("target/debug/dep.rlib").is_file());
    assert!(shared.join("debug/dep.rlib").is_file());
    // `--reclaim-plan` without `--host` is a usage error.
    let (code, _, _) = s.cli_at(&s.repo, &["doctor", "--reclaim-plan"]);
    assert_ne!(code, 0);
}

/// The r3 acceptance case: the emitted shared-cache command is run
/// through a real `sh` — it must empty the shared subdirs while
/// leaving the directories themselves in place, so every lane's
/// symlinks keep resolving and the lane still builds afterwards.
/// (The r2 command deleted the dirs outright; every lane then died
/// with `File exists (os error 17)` on its next build.)
#[test]
fn reclaim_plan_command_keeps_lanes_buildable() {
    let s = SharedTarget::new();
    s.new_issue("Warm");
    let (ok, _) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok);
    let wt = s.worktree_of("D-1");
    s.set_marker(&wt, "warm");
    s.cargo_build(&wt);
    let shared_debug = s.repo.join(".cadence/target/shared/debug");
    // Deps really landed in the cache through the links.
    assert!(std::fs::read_dir(shared_debug.join("deps"))
        .unwrap()
        .next()
        .is_some());

    let (_code, stdout, _) = s.cli_at(&s.repo, &["doctor", "--host", "--reclaim-plan", "--json"]);
    let report: Value = serde_json::from_str(stdout.trim()).unwrap();
    let row = report["reclaim"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "shared-cargo-cache")
        .expect("shared row");
    let action = row["action"].as_str().unwrap().to_string();
    assert!(action.contains("rm -rf"), "{action}");
    // Run exactly what the plan prints — comment and all — via `sh`.
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&action)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{action}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The symlink targets must still exist — dangling links are the
    // bug this round fixes.
    for name in ["deps", ".fingerprint", "build", "incremental"] {
        let dir = shared_debug.join(name);
        assert!(dir.is_dir(), "{name} deleted by the reclaim command");
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "{name} should be emptied"
        );
        // And the lane's link resolves to a real dir, not dangling.
        assert!(
            wt.join("target/debug").join(name).is_dir(),
            "{name} dangles"
        );
    }
    // The lane still builds — cargo recreates what it needs inside
    // the surviving dirs.
    s.cargo_build(&wt);
    let bin = wt.join("target/debug/marker");
    let out = std::process::Command::new(&bin).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "warm");
}

/// `CARGO_TARGET_DIR` outranks every config file — with it exported,
/// a planted farm would sit inert while every lane collided in the
/// env dir, so `issue start` plants nothing and records the env's
/// dir on the worktree ref.
#[test]
fn issue_start_honours_cargo_target_dir_env() {
    let s = SharedTarget::new();
    s.new_issue("Env");
    let envdir = s.repo.join("env-target");
    let (code, stdout, stderr) = s.cli_at_env(
        &s.repo,
        &["issue", "start", "D-1"],
        &[("CARGO_TARGET_DIR", envdir.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{stderr}");
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        out["target_dir"].as_str().unwrap(),
        envdir.to_string_lossy(),
        "{out}"
    );
    // No farm — every build lands in the env dir instead.
    let wt = s.worktree_of("D-1");
    assert!(!wt.join("target/debug/deps").exists());
    let show = s.cli(&["issue", "show", "D-1", "--json"]).1;
    let wt_ref = show["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "worktree")
        .unwrap();
    assert_eq!(
        wt_ref["cargo_target"].as_str().unwrap(),
        envdir.to_string_lossy()
    );
}

fn stub_agent(
    alias: &str,
    provider: &str,
    kind: &str,
    state: &str,
    idle_for_secs: i64,
    show: (Vec<Value>, i64, i64),
) -> StubAgent {
    let (messages, queued, unknown) = show;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    StubAgent {
        row: json!({
            "alias": alias, "provider": provider, "endpoint_kind": kind,
            "role": "", "cwd": "/", "state": state, "enabled": true,
            "dead": false, "endpoint": if state == "stopped" { Value::Null } else { json!("ep") },
            "created": now - 86_400.0, "updated": now - idle_for_secs as f64,
        }),
        messages,
        queued,
        unknown,
        flip: None,
    }
}

fn stub_daemon(state: &Path, build_commit: &str, agents: Vec<StubAgent>) -> StubDaemon {
    std::fs::create_dir_all(state).unwrap();
    let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
    let calls = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let calls_t = Arc::clone(&calls);
    let rows: Vec<Value> = agents.iter().map(|a| a.row.clone()).collect();
    let mut shows: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut flips: std::collections::HashMap<String, (Arc<Mutex<u32>>, Value)> =
        std::collections::HashMap::new();
    for a in agents {
        let alias = a.row["alias"].as_str().unwrap().to_string();
        shows.insert(
            alias.clone(),
            json!({
                "agent": a.row, "messages": a.messages,
                "queued": a.queued, "unknown": a.unknown,
                "event_cursor": 0,
            }),
        );
        if let Some(flip) = a.flip {
            flips.insert(alias, (Arc::new(Mutex::new(0)), flip));
        }
    }
    let info = json!({
        "build_commit": build_commit,
        "build_time": "2026-01-01T00:00:00Z",
        "started_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - 600,
    });
    let thread = thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let req: Value = serde_json::from_str(&line).unwrap_or_default();
            let method = req["method"].as_str().unwrap_or_default().to_string();
            calls_t
                .lock()
                .unwrap()
                .push((method.clone(), req["params"].clone()));
            let params = &req["params"];
            let result = match method.as_str() {
                "health" => json!({"ok": true, "version": 1}),
                "daemon_info" => info.clone(),
                "agent_list" => json!({"agents": rows}),
                "agent_show" => {
                    let alias = params["alias"].as_str().unwrap_or_default();
                    if let Some((n, flip)) = flips.get(alias) {
                        let mut n = n.lock().unwrap();
                        *n += 1;
                        if *n >= 2 {
                            flip.clone()
                        } else {
                            shows.get(alias).cloned().unwrap_or_default()
                        }
                    } else {
                        shows
                            .get(alias)
                            .cloned()
                            .unwrap_or_else(|| json!({"messages": [], "queued": 0, "unknown": 0}))
                    }
                }
                "agent_requests" => json!({"requests": []}),
                "agent_probe" => json!({"idle": true}),
                "agent_stop" => json!({"alias": params["alias"], "state": "stopped"}),
                "agent_gc" => json!({"removed": []}),
                _ => {
                    let _ = writeln!(
                        stream,
                        "{}",
                        json!({"ok": false, "error": {"kind": "rejected",
                              "message": format!("Unknown method {method}")}})
                    );
                    continue;
                }
            };
            let _ = writeln!(stream, "{}", json!({"ok": true, "result": result}));
        }
    });
    StubDaemon {
        calls,
        _thread: thread,
    }
}

fn stub_calls(sd: &StubDaemon) -> Vec<(String, Value)> {
    sd.calls.lock().unwrap().clone()
}

/// `<pm>` with one project pointing at `repo`, one `doing` issue
/// owned by an alias the stub does not serve, a second `doing` issue
/// with branch+worktree refs to `repo`'s merged `.cadence/wt/tst-7-done`
/// (what `issue start` records), and an empty notes dir. The pm dir is
/// a git repo — `issue finish` commits ref-closures into it.
fn seed_pm(pm: &Path, repo: &Path, notes: &Path) {
    std::fs::create_dir_all(pm.join("tst")).unwrap();
    std::fs::create_dir_all(notes).unwrap();
    std::fs::write(
        pm.join("pm.yaml"),
        format!(
            "schema: 1\nnotes_dir: {}\nstatuses:\n- backlog\n- ready\n- doing\n- review\n- done\n- dropped\n",
            notes.display()
        ),
    )
    .unwrap();
    std::fs::write(
        pm.join("tst/project.yaml"),
        format!(
            "key: tst\nprefix: TST\nrepos:\n- path: {}\n",
            repo.display()
        ),
    )
    .unwrap();
    let issue = pm.join("tst/TST-9");
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(
        issue.join("issue.md"),
        "---\nid: TST-9\ntitle: ghost-owned doing issue\nstatus: doing\n\
         priority: P2\nowner: ghost-agent\ncreated: 2026-01-01T00:00:00Z\n---\n\nbody\n",
    )
    .unwrap();
    let wt = repo.join(".cadence/wt/tst-7-done");
    let done = pm.join("tst/TST-7");
    std::fs::create_dir_all(&done).unwrap();
    std::fs::write(
        done.join("issue.md"),
        format!(
            "---\nid: TST-7\ntitle: merged worktree\nstatus: doing\npriority: P2\n\
             created: 2026-01-01T00:00:00Z\nrefs:\n\
             - kind: branch\n  path: cadence/tst-7-done\n\
             - kind: worktree\n  path: {}\n---\n\nbody\n",
            wt.display()
        ),
    )
    .unwrap();
    // `issue finish` commits the ref-closure — the pm dir must be a
    // real git repo with an identity.
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "seed"],
    ] {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(pm)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

/// A git repo with a merged `cadence/tst-7-done` worktree and a plain
/// `.cadence/wt/tst-88-ghost` dir — one merge candidate, one orphan.
fn seed_repo(repo: &Path) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::create_dir_all(repo).unwrap();
    git(&["init", "-b", "main"]);
    git(&["config", "user.email", "test@x"]);
    git(&["config", "user.name", "test"]);
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "init"]);
    git(&["branch", "cadence/tst-7-done"]);
    git(&[
        "worktree",
        "add",
        ".cadence/wt/tst-7-done",
        "cadence/tst-7-done",
    ]);
    // Real work, fast-forward merged, then left idle — a lane with no
    // commits has not started and is never merged (CAD-275).
    let wt = repo.join(".cadence/wt/tst-7-done");
    let wt_s = wt.to_str().unwrap();
    std::fs::write(wt.join("done.txt"), "done\n").unwrap();
    git(&["-C", wt_s, "add", "-A"]);
    git(&["-C", wt_s, "commit", "-m", "tst-7 work"]);
    git(&["merge", "--ff-only", "cadence/tst-7-done"]);
    idle(&wt);
    std::fs::create_dir_all(repo.join(".cadence/wt/tst-88-ghost")).unwrap();
}

/// `cadence <args>` against the fixture state dir + pm dir, cwd at the
/// fixture repo. Output captured; the stub answers daemon RPCs.
fn run_session(state: &Path, pm: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    run_session_env(state, pm, cwd, args, &[])
}

/// `run_session` with extra env pairs — the host-report fixture and a
/// stubbed `gh` on PATH ride in through here.
fn run_session_env(
    state: &Path,
    pm: &Path,
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &Path)],
) -> std::process::Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .current_dir(cwd);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// `run_session` with `--host-report <fixture>` appended — the flag,
/// never an env var, carries the fixture so an ambient environment
/// cannot soften the gate.
fn run_session_host(
    state: &Path,
    pm: &Path,
    cwd: &Path,
    args: &[&str],
    host: &Path,
    envs: &[(&str, &Path)],
) -> std::process::Output {
    let mut v: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    v.push("--host-report".to_string());
    v.push(host.display().to_string());
    let argrefs: Vec<&str> = v.iter().map(|a| a.as_str()).collect();
    run_session_env(state, pm, cwd, &argrefs, envs)
}

/// A clean doctor-host report on disk — `--host-report` makes the
/// verbs read it instead of scanning the real host, so the session
/// tests are identical on a dev box and a 97%-full CI host.
fn clean_host(dir: &Path) -> PathBuf {
    let f = dir.join("host-report.json");
    std::fs::write(&f, r#"{"level":"ok","checks":[]}"#).unwrap();
    f
}

#[test]
fn session_start_reports_failures_and_fix_only_starts_ui() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    // `--fix` starts a daemon if the stub ever stops answering.
    let _reaper = DaemonReaper::new(&state);
    let pm = dir.path().join("pm");
    let repo = dir.path().join("repo");
    seed_pm(&pm, &repo, &dir.path().join("notes"));
    seed_repo(&repo);
    let sd = stub_daemon(
        &state,
        "stale-build-000",
        vec![
            stub_agent(
                "w1",
                "fake",
                "fake",
                "attention",
                700,
                (
                    vec![json!({"id": "m-unk", "state": "unknown", "body": "lost turn"})],
                    0,
                    1,
                ),
            ),
            stub_agent(
                "pm-inbox",
                "inbox",
                "inbox",
                "idle",
                700,
                (
                    vec![json!({"id": "k1", "state": "queued", "body": "kickoff"})],
                    2,
                    0,
                ),
            ),
        ],
    );

    let host = clean_host(dir.path());
    let out = run_session_host(&state, &pm, &repo, &["session", "start"], &host, &[]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(2), "expected no-go exit 2:\n{text}");
    assert!(
        text.contains("stale-build-000"),
        "stale daemon build:\n{text}"
    );
    assert!(text.contains("m-unk"), "unknown message named:\n{text}");
    assert!(text.contains("tst-88-ghost"), "orphan worktree:\n{text}");
    assert!(text.contains("pm-inbox"), "unread inbox:\n{text}");
    assert!(
        text.contains("TST-9"),
        "doing issue with dead owner:\n{text}"
    );
    // Nothing was fixed or mutated without --fix.
    assert!(!state.join("ui.pid").exists());
    for (m, _) in stub_calls(&sd) {
        assert!(
            !matches!(m.as_str(), "agent_stop" | "agent_gc"),
            "read-only start mutated: {m}"
        );
    }

    // --fix: a free port persisted in ui.json lets the real binary
    // spawn `ui run`; the stub daemon must be left untouched.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    std::fs::write(state.join("ui.json"), format!("{{\"port\": {port}}}")).unwrap();
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "start", "--fix"],
        &host,
        &[],
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !state.join("ui.pid").exists() {
        assert!(Instant::now() < deadline, "ui never started:\n{text}");
        thread::sleep(Duration::from_millis(100));
    }
    // Nothing else: the daemon was reachable, so no daemon start (the
    // log file it would create is absent) and no mutating RPCs.
    assert!(
        !state.join("daemon.log").exists(),
        "daemon was started:\n{text}"
    );
    for (m, _) in stub_calls(&sd) {
        assert!(
            !matches!(m.as_str(), "agent_stop" | "agent_gc" | "agent_send"),
            "--fix mutated: {m}"
        );
    }
    let stop = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["ui", "stop"])
        .output()
        .unwrap();
    assert!(stop.status.success());
}

#[test]
fn session_end_dry_run_plans_real_run_stops_only_idle() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    let repo = dir.path().join("repo");
    seed_pm(&pm, &repo, &dir.path().join("notes"));
    seed_repo(&repo);
    let sd = stub_daemon(
        &state,
        cadence_agent::overview::BUILD_COMMIT,
        vec![
            stub_agent("old-idle", "fake", "fake", "idle", 7200, (vec![], 0, 0)),
            stub_agent("fresh-idle", "fake", "fake", "idle", 60, (vec![], 0, 0)),
            // `state: idle` with a running message — the message, not
            // the state field, is what keeps it alive (running_msg).
            stub_agent(
                "idle-running",
                "fake",
                "fake",
                "idle",
                7200,
                (
                    vec![json!({"id": "m-ir", "state": "running",
                             "body": "claimed mid-run", "started": 1.0})],
                    0,
                    0,
                ),
            ),
            stub_agent(
                "busy-one",
                "fake",
                "fake",
                "busy",
                7200,
                (
                    vec![json!({"id": "m-run", "state": "running",
                             "body": "long turn", "started": 1.0})],
                    0,
                    0,
                ),
            ),
            stub_agent(
                "queued-one",
                "fake",
                "fake",
                "idle",
                7200,
                (
                    vec![json!({"id": "m-q", "state": "queued", "body": "queued"})],
                    1,
                    0,
                ),
            ),
            stub_agent("pm-inbox", "inbox", "inbox", "idle", 7200, (vec![], 2, 0)),
        ],
    );
    let host = clean_host(dir.path());
    // A stub `gh` first on PATH: a dry run must never invoke it —
    // the gh cache is read, not refreshed.
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh_called = dir.path().join("gh-called");
    std::fs::write(
        bin.join("gh"),
        format!(
            "#!/bin/sh\necho called >> '{}'\nexit 1\n",
            gh_called.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let envs = [("PATH", Path::new(&path_env))];

    // Dry run: names the stop candidates and the merged worktree,
    // changes nothing.
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--dry-run"],
        &host,
        &envs,
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("old-idle"),
        "dry-run names idle agent:\n{text}"
    );
    assert!(
        text.contains("tst-7-done"),
        "dry-run names merged worktree:\n{text}"
    );
    assert!(
        !text.contains("fresh-idle"),
        "dry-run must not list a still-active agent:\n{text}"
    );
    for (m, _) in stub_calls(&sd) {
        assert!(
            !matches!(m.as_str(), "agent_stop" | "agent_gc"),
            "dry-run mutated: {m}"
        );
    }
    // A dry run writes nothing — no sessions/ dir, and the markdown is
    // previewed to stdout instead.
    let sessions = state.join("sessions");
    assert!(
        !sessions.exists() || std::fs::read_dir(&sessions).unwrap().next().is_none(),
        "dry-run wrote a handoff file: {:?}",
        sessions
    );
    assert!(
        text.contains("would write") && text.contains("## open PRs"),
        "dry-run previews the handoff, writes nothing:\n{text}"
    );
    // Nothing at all was written — no handoff, no gh cache, no `gh`
    // subprocess at all.
    assert!(
        !gh_called.exists(),
        "dry-run invoked gh — a dry run writes nothing, cache included"
    );
    assert!(
        !state.join("overview-gh.json").exists(),
        "dry-run wrote the gh cache"
    );

    // Real run: only the agent idle past --idle-secs is stopped.
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--idle-secs", "1800"],
        &host,
        &envs,
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stops: Vec<(String, Value)> = stub_calls(&sd)
        .into_iter()
        .filter(|(m, _)| m == "agent_stop")
        .collect();
    assert_eq!(stops.len(), 1, "exactly one stop:\n{text}");
    assert_eq!(
        stops[0].1["alias"].as_str().unwrap_or_default(),
        "old-idle",
        "the stop names only the idle agent:\n{text}"
    );
    // `idle-running` was state-idle but carries a running message —
    // never stopped.
    for (m, p) in stub_calls(&sd) {
        if m == "agent_stop" {
            assert_ne!(
                p["alias"].as_str().unwrap_or_default(),
                "idle-running",
                "an agent with a running message was stopped:\n{text}"
            );
        }
    }
    assert!(
        stub_calls(&sd).iter().any(|(m, _)| m == "agent_gc"),
        "agent gc ran:\n{text}"
    );
    assert!(text.contains("old-idle"));
    // S6: the real-run candidate count is finished+refused — the same
    // set the dry run counts, not every sweep row incl. skips.
    assert!(
        text.contains("1 finished of 1 candidate(s)"),
        "finish count uses the same set dry-run does:\n{text}"
    );

    // The handoff note landed with the required sections.
    let sessions = state.join("sessions");
    let notes: Vec<PathBuf> = std::fs::read_dir(&sessions)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(notes.len(), 1, "one handoff file: {notes:?}");
    let md = std::fs::read_to_string(&notes[0]).unwrap();
    for section in [
        "## open PRs",
        "## running turns",
        "## queued kickoffs",
        "## issues in review",
        "## done this run",
        "## next session first",
    ] {
        assert!(md.contains(section), "handoff missing {section}:\n{md}");
    }
    assert!(md.contains("old-idle"), "stopped agent recorded:\n{md}");

    // A second real run the same day never overwrites the first.
    let out = run_session_host(&state, &pm, &repo, &["session", "end"], &host, &envs);
    // The host fixture is clean and nothing failed — this is a green
    // run outright, on any host.
    assert!(
        out.status.success(),
        "second end failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let notes: Vec<PathBuf> = std::fs::read_dir(&sessions)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(
        notes.len(),
        2,
        "two handoff files, none overwritten: {notes:?}"
    );
}

/// Round-2 B1: the initial `fleet()` snapshot is only a candidate list.
/// If an agent turns busy between the snapshot and the stop, `session
/// end` re-shows it and must not stop it.
#[test]
fn session_end_stop_race_rechecks_show() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    // First agent_show: an idle, stop-worthy candidate. Second show —
    // the pre-stop re-check — the agent has turned busy with a running
    // message. No agent_stop may be issued for it.
    let flip_show = json!({
        "agent": {
            "alias": "racy", "provider": "fake", "endpoint_kind": "fake",
            "endpoint": "ep", "state": "busy", "dead": false,
            "updated": now,
        },
        "messages": [{"id": "m-racy", "state": "running",
                      "body": "dispatched mid-sweep", "started": now}],
        "queued": 0, "unknown": 0, "event_cursor": 0,
    });
    let sd = stub_daemon(
        &state,
        cadence_agent::overview::BUILD_COMMIT,
        vec![
            stub_agent("racy", "fake", "fake", "idle", 7200, (vec![], 0, 0)).flipping(flip_show),
            stub_agent("calm", "fake", "fake", "idle", 7200, (vec![], 0, 0)),
        ],
    );
    let host = clean_host(tmp.path());
    let out = run_session_host(&state, &pm, &repo, &["session", "end"], &host, &[]);
    assert!(
        out.status.success(),
        "end failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let stops: Vec<(String, Value)> = stub_calls(&sd)
        .into_iter()
        .filter(|(m, _)| m == "agent_stop")
        .collect();
    assert_eq!(
        stops.len(),
        1,
        "only the still-idle agent was stopped:\n{text}"
    );
    assert_eq!(
        stops[0].1["alias"].as_str().unwrap_or_default(),
        "calm",
        "the raced agent must never be stopped:\n{text}"
    );
    assert!(
        text.contains("racy") && text.contains("skip"),
        "the skipped re-check is reported:\n{text}"
    );
    // ...once: a skipped candidate is one annotated row, not a bare
    // candidate line plus a second `— skipped` line.
    assert_eq!(
        text.matches("racy").count(),
        1,
        "the skipped alias listed twice:\n{text}"
    );
    // Contract: the re-check was a second agent_show for racy.
    let shows = stub_calls(&sd)
        .iter()
        .filter(|(m, p)| m == "agent_show" && p["alias"] == "racy")
        .count();
    assert!(
        shows >= 2,
        "racy was re-shown {shows}x before the stop decision"
    );
}

/// Round-2 B3: --project scopes the finish sweep; other projects'
/// merged worktrees are never touched.
#[test]
fn session_end_project_scopes_sweep() {
    let tmp = TempDir::new().unwrap();
    let (pm_dir, home, state) = (
        tmp.path().join("pm"),
        tmp.path().join("home"),
        tmp.path().join("state"),
    );
    let repo_a = tmp.path().join("repo-a");
    let repo_b = tmp.path().join("repo-b");
    for dir in [&pm_dir, &home, &state, &repo_a, &repo_b] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = git_ok();
    for repo in [&repo_a, &repo_b] {
        git(repo, &["init", "-b", "main"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "init"]);
    }
    let cli_raw = cadence_cli_raw(&state, &pm_dir, &home);
    let cli = |args: &[&str]| -> (bool, Value) {
        let (code, stdout, stderr) = cli_raw(args);
        (
            code == 0,
            serde_json::from_str(stdout.trim())
                .unwrap_or_else(|_| panic!("{args:?} not json ({stderr}): {stdout}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let ra = repo_a.canonicalize().unwrap().to_str().unwrap().to_string();
    let rb = repo_b.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "aaa", "--prefix", "A", "--repo", &ra]).0);
    assert!(cli(&["issue", "project", "add", "bbb", "--prefix", "B", "--repo", &rb]).0);
    assert!(cli(&["issue", "new", "one", "--project", "aaa"]).0);
    assert!(cli(&["issue", "new", "two", "--project", "bbb"]).0);
    for id in ["A-1", "B-1"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    // A-1's owner is the second way an agent belongs to the project —
    // cwd-under-repo is the first.
    let (ok, out) = cli(&["issue", "set", "A-1", "owner=a-owned"]);
    assert!(ok, "{out}");
    let wt_a = repo_a.join(".cadence/wt/a-1-one");
    let wt_b = repo_b.join(".cadence/wt/b-1-two");
    for (wt, file) in [(&wt_a, "a.txt"), (&wt_b, "b.txt")] {
        std::fs::write(wt.join(file), "x").unwrap();
        git(wt, &["add", "-A"]);
        git(wt, &["commit", "-qm", "work"]);
        idle(wt);
    }
    git(&repo_a, &["merge", "-q", "cadence/a-1-one"]);
    git(&repo_b, &["merge", "-q", "cadence/b-1-two"]);
    assert!(wt_a.exists() && wt_b.exists(), "fixture wts exist");

    // Fleet: `a-idle` works under repo_a, `a-owned` owns A-1 outright,
    // `b-idle` works under repo_b — all idle past the threshold.
    let sd = stub_daemon(
        &state,
        cadence_agent::overview::BUILD_COMMIT,
        vec![
            stub_agent("a-idle", "fake", "fake", "idle", 7200, (vec![], 0, 0))
                .with_cwd(&repo_a.canonicalize().unwrap()),
            stub_agent("a-owned", "fake", "fake", "idle", 7200, (vec![], 0, 0)),
            stub_agent("b-idle", "fake", "fake", "idle", 7200, (vec![], 0, 0))
                .with_cwd(&repo_b.canonicalize().unwrap()),
        ],
    );
    let host = clean_host(tmp.path());
    let out = run_session_host(
        &state,
        &pm_dir,
        &repo_a,
        &["session", "end", "--project", "aaa"],
        &host,
        &[],
    );
    assert!(
        out.status.success(),
        "scoped end failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !wt_a.exists(),
        "scoped run left project aaa's merged worktree"
    );
    assert!(wt_b.exists(), "scoped run removed project bbb's worktree");
    // --project scopes the stops too: aaa's agents stopped, bbb's left
    // running and reported as out-of-scope.
    let mut stopped: Vec<String> = stub_calls(&sd)
        .into_iter()
        .filter(|(m, _)| m == "agent_stop")
        .filter_map(|(_, p)| p["alias"].as_str().map(str::to_string))
        .collect();
    stopped.sort();
    assert_eq!(stopped, vec!["a-idle", "a-owned"], "scoped stops:\n{text}");
    assert!(
        text.contains("outside project 'aaa'"),
        "the out-of-scope agent is reported:\n{text}"
    );
    // `agent_gc` is fleet-wide — under --project it is skipped, and the
    // row says so rather than overreaching into bbb's agents.
    assert!(
        !stub_calls(&sd).iter().any(|(m, _)| m == "agent_gc"),
        "fleet-wide gc ran under --project:\n{text}"
    );
    assert!(
        text.contains("gc is fleet-wide — skipped under --project aaa"),
        "the gc row says which:\n{text}"
    );
}

/// Round-2 S7 + nit: `--json` stdout is exactly one JSON document, and
/// an unknown `--project` is rejected like `issue ls --project`.
#[test]
fn session_json_single_document_and_project_validation() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    // `--fix` starts a daemon if the stub ever stops answering.
    let _reaper = DaemonReaper::new(&state);
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    let _sd = stub_daemon(&state, cadence_agent::overview::BUILD_COMMIT, vec![]);
    // `ui.json` seeded with a free port so `start --fix` actually starts
    // the UI — the path that used to pollute the composed JSON.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    std::fs::write(state.join("ui.json"), format!("{{\"port\": {port}}}")).unwrap();
    let host = clean_host(tmp.path());

    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "start", "--fix", "--json"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<Value>(text.trim())
        .unwrap_or_else(|e| panic!("start --fix --json is not one document: {e}\n{text}"));
    // `--fix` actually started the UI — without this the test goes
    // vacuous: a silently no-op fix still prints one clean document.
    assert!(
        state.join("ui.pid").exists(),
        "start --fix did not start the UI (no ui.pid):\n{text}"
    );
    let _ = run_session(&state, &pm, &repo, &["ui", "stop"]);

    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--json"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<Value>(text.trim())
        .unwrap_or_else(|e| panic!("end --json is not one document: {e}\n{text}"));

    // Unknown project is an error, not an empty sweep.
    for args in [
        vec!["session", "start", "--project", "nosuch"],
        vec!["session", "end", "--project", "nosuch"],
    ] {
        let out = run_session_host(&state, &pm, &repo, &args, &host, &[]);
        assert!(
            !out.status.success(),
            "{args:?} accepted an unknown project:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let msg = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(msg.contains("nosuch"), "{args:?}: {msg}");
    }
}

/// Round-3 B1: the host sweep in `session end` is a report, not a
/// gate — a `fail`-level host caps at `warn` (exit 1) while `session
/// start` correctly treats the same host as a no-go (exit 2). The
/// fixture pins the state; the real host is never read.
#[test]
fn session_end_host_fail_caps_at_warn() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    seed_repo(&repo);
    let _sd = stub_daemon(&state, cadence_agent::overview::BUILD_COMMIT, vec![]);
    // A `fail` host — the 97%-full disk that broke these tests.
    let host = tmp.path().join("host-fail.json");
    std::fs::write(
        &host,
        r#"{"level":"fail","checks":[{"name":"disk","level":"fail",
        "detail":"/ 97% full","remedy":"clean up"}]}"#,
    )
    .unwrap();
    let out = run_session_host(&state, &pm, &repo, &["session", "start"], &host, &[]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "start must still no-go on a fail host:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = run_session_host(&state, &pm, &repo, &["session", "end"], &host, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "end caps the host report at warn, exit 1:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("sweep     warn") || text.contains("sweep warn"),
        "the sweep row shows warn, not fail:\n{text}"
    );
}

/// Addendum §1: session output never prints process argv — every
/// scrubber leaks some shape, so orphans display as `exe (arg count)`.
/// The fixture plants a credential in `head`; it must not appear.
/// `pid = self` gives a readable cmdline; `u32::MAX` gives the
/// unavailable path.
#[test]
fn session_end_orphans_report_exe_and_argc_never_argv() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    seed_repo(&repo);
    let _sd = stub_daemon(&state, cadence_agent::overview::BUILD_COMMIT, vec![]);
    let me = std::process::id();
    let host = tmp.path().join("host-orphans.json");
    std::fs::write(
        &host,
        format!(
            r#"{{"level":"warn","checks":[{{"name":"orphans","level":"warn",
            "detail":"2: pid {me} (1h npm exec --api-key=figd_PLANTEDLEAK --stdio)",
            "remedy":"kill {me} 4294967295",
            "value":{{"pids":[
                {{"pid":{me},"head":"npm exec --api-key=figd_PLANTEDLEAK --stdio",
                  "reasons":["cwd/exe under a deleted .cadence/wt worktree"]}},
                {{"pid":4294967295,"head":"./hung-test-binary --secret=hunter2",
                  "reasons":["cargo test binary older than an hour"]}}
            ]}}}}]}}"#
        ),
    )
    .unwrap();
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--dry-run"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("orphans: 2 orphaned pid(s)"),
        "count-only detail replaces the argv-bearing one:\n{text}"
    );
    let exe = std::env::current_exe()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        text.contains(&format!("orphan pid {me} — {exe}")),
        "self pid shows the executable basename:\n{text}"
    );
    assert!(
        text.contains(&format!("orphan pid {me} — {exe}")) && text.contains(" arg(s))"),
        "the argument count is shown:\n{text}"
    );
    assert!(
        text.contains("orphan pid 4294967295 — (argv unavailable)"),
        "an unreadable cmdline degrades without head:\n{text}"
    );
    for leaked in [
        "figd_PLANTEDLEAK",
        "hunter2",
        "--api-key",
        "--secret",
        "npm exec",
        "hung-test-binary",
    ] {
        assert!(!text.contains(leaked), "argv leaked as {leaked:?}:\n{text}");
    }
    assert_eq!(
        out.status.code(),
        Some(1),
        "warn host, dry-run clean: exit 1:\n{text}"
    );
}

/// Round-4 blocker: the host fixture is `--host-report`, never an env
/// var — an ambient `CADENCE_SESSION_HOST_JSON` must not soften the
/// gate, a bad fixture path is a hard error, and every fixture run is
/// labelled in text and `--json`.
#[test]
fn session_host_report_flag_labels_errors_and_env_is_dead() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    seed_repo(&repo);
    let _sd = stub_daemon(&state, cadence_agent::overview::BUILD_COMMIT, vec![]);

    // A bad fixture path errors — it never falls through to a real
    // scan (that would read the real host with no signal).
    let out = run_session(
        &state,
        &pm,
        &repo,
        &["session", "end", "--host-report", "/nonexistent/host.json"],
    );
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success() && msg.contains("/nonexistent/host.json"),
        "a bad fixture path must error naming the path:\n{msg}"
    );
    // …before any mutation — no handoff was written.
    assert!(
        !state.join("sessions").exists()
            || std::fs::read_dir(state.join("sessions"))
                .unwrap()
                .next()
                .is_none(),
        "a bad fixture must fail before the handoff write"
    );
    // Same for a parsable-path-but-not-JSON file.
    let garbage = tmp.path().join("not-json.json");
    std::fs::write(&garbage, "not json at all").unwrap();
    let out = run_session(
        &state,
        &pm,
        &repo,
        &["session", "end", "--host-report", garbage.to_str().unwrap()],
    );
    assert!(
        !out.status.success(),
        "an unparsable fixture must error:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A fixture run is labelled — in text and in --json.
    let host = clean_host(tmp.path());
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--dry-run"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("real host not scanned"),
        "text output labels the fixture:\n{text}"
    );
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--dry-run", "--json"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("end --json not one document: {e}\n{text}"));
    assert_eq!(
        j["host_source"].as_str().unwrap_or_default(),
        format!("fixture {}", host.display()),
        "--json labels the fixture:\n{text}"
    );
    // `session start` takes the same flag and labels it the same way.
    let out = run_session_host(&state, &pm, &repo, &["session", "start"], &host, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("real host not scanned"),
        "session start labels the fixture:\n{text}"
    );
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "start", "--json"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("start --json not one document: {e}\n{text}"));
    assert_eq!(
        j["host_source"].as_str().unwrap_or_default(),
        format!("fixture {}", host.display()),
        "start --json labels the fixture:\n{text}"
    );

    // The old env var is dead: set it to a *failing* fixture and run
    // without the flag — the real host is scanned, the sentinel
    // detail never appears, and host_source reports `scan`.
    let sentinel = tmp.path().join("env-fixture.json");
    std::fs::write(
        &sentinel,
        r#"{"level":"fail","checks":[{"name":"disk","level":"fail",
        "detail":"SENTINEL-DISK-SHOULD-NEVER-APPEAR","remedy":"x"}]}"#,
    )
    .unwrap();
    let out = run_session_env(
        &state,
        &pm,
        &repo,
        &["session", "end", "--json"],
        &[("CADENCE_SESSION_HOST_JSON", sentinel.as_path())],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("SENTINEL-DISK-SHOULD-NEVER-APPEAR"),
        "the env var must have no effect — it read the fixture:\n{text}"
    );
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("end --json not one document: {e}\n{text}"));
    assert_eq!(
        j["host_source"].as_str().unwrap_or_default(),
        "scan",
        "env-set fixture must report a real scan:\n{text}"
    );
}

/// CAD-146: piping output into a reader that closes early
/// (`cadence … | head -12`) must exit 0 quietly. Rust ignores
/// SIGPIPE, so the closed read end turns the next stdout write into
/// EPIPE and `println!` panics — the startup panic hook turns exactly
/// that failure into exit 0. The read end is dropped before the
/// first write so the broken pipe is deterministic.
#[test]
fn issue_ls_survives_a_closed_downstream_pipe() {
    let tmp = TempDir::new().unwrap();
    let (pm_dir, home, state) = (
        tmp.path().join("pm"),
        tmp.path().join("home"),
        tmp.path().join("state"),
    );
    for d in [&pm_dir, &home, &state] {
        std::fs::create_dir_all(d).unwrap();
    }
    issue_cli(&home, &state, &pm_dir, &["issue", "init"]);
    issue_cli(
        &home,
        &state,
        &pm_dir,
        &["issue", "project", "add", "demo", "--prefix", "D"],
    );
    // Enough issues that the listing overflows the 64KiB pipe buffer
    // even if the drop raced the first writes — seeded directly since
    // 400 `issue new`s would spend the test in pm git commits.
    for i in 1..=400 {
        let dir = pm_dir.join("demo").join(format!("D-{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("issue.md"),
            format!(
                "---\nid: D-{i}\ntitle: a reasonably long issue title \
                 carrying some weight {i}\nstatus: backlog\npriority: P2\n\
                 created: 2026-09-20T00:00:00Z\n---\n\nbody\n"
            ),
        )
        .unwrap();
    }
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["issue", "ls", "--json"])
        .env("HOME", &home)
        .env("CADENCE_PM_DIR", &pm_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // The downstream reader is gone before the listing starts.
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a closed pipe must exit 0, not panic or die by signal: {stderr}"
    );
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "the EPIPE must never reach the user: {stderr}"
    );

    // But a verb that FAILS must keep its real exit code — the hook
    // exits with the code the process already committed to, not a
    // hard-coded 0. `issue show NOSUCH` writes its error to stderr;
    // with both stream ends closed (`2>&1 | head -c 0`) that print
    // panics on EPIPE and the answer must still be failure.
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["issue", "show", "NOSUCH-9999"])
        .env("HOME", &home)
        .env("CADENCE_PM_DIR", &pm_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    drop(child.stderr.take());
    let status = child.wait().unwrap();
    assert_eq!(
        status.code(),
        Some(1),
        "a failing verb keeps its failure code on a closed pipe"
    );
    // …and with the reader still open, the same failure exits the
    // same way — the hook only fires when the pipe is gone.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["issue", "show", "NOSUCH-9999"])
        .env("HOME", &home)
        .env("CADENCE_PM_DIR", &pm_dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("NOSUCH-9999"),
        "the open-pipe failure still prints its error"
    );
}

/// `issue start` writes the worktree slot env: `CARGO_BUILD_JOBS` from
/// `[host] jobs_per_lane` plus the helper path — idempotent, and a
/// foreign line in an existing `.env` survives.
#[test]
fn issue_start_writes_slot_env() {
    let d = TestDaemon::start();
    let (_tmp, pm_dir, repo, home) = pm_lab_dirs();

    let git = git_ok();
    git_f_repo(&repo, &git, |_| {});
    let cli = cadence_cli_json(&d.state, &pm_dir, &home);
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]).0);
    assert!(cli(&["issue", "new", "One", "--project", "demo"]).0);
    // The [host] override lands before the start reads it.
    let pm_yaml = pm_dir.join("pm.yaml");
    let mut yaml = std::fs::read_to_string(&pm_yaml).unwrap();
    yaml.push_str("host:\n  jobs_per_lane: 7\n");
    std::fs::write(&pm_yaml, yaml).unwrap();
    let (ok, out) = cli(&["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let env_file = PathBuf::from(out["slot_env"]["path"].as_str().unwrap());
    assert_eq!(
        env_file,
        Path::new(out["worktree"].as_str().unwrap()).join(".env")
    );
    let text = std::fs::read_to_string(&env_file).unwrap();
    assert!(text.contains("CARGO_BUILD_JOBS=7"), "{text}");
    assert!(text.contains("CADENCE_BUILD_SLOT="), "{text}");
    assert!(text.contains("cadence"), "{text}");
    // Created 0600 — the file may hold build secrets someday.
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&env_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // A second start is idempotent and keeps foreign lines — and an
    // existing file's mode survives the atomic rewrite.
    std::fs::write(&env_file, format!("OTHER=1\n{text}")).unwrap();
    std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let (ok, _) = cli(&["issue", "start", "D-1"]);
    assert!(ok);
    let text = std::fs::read_to_string(&env_file).unwrap();
    assert_eq!(text.matches("CARGO_BUILD_JOBS=").count(), 1, "{text}");
    assert!(text.contains("OTHER=1"), "{text}");
    assert_eq!(
        std::fs::metadata(&env_file).unwrap().permissions().mode() & 0o777,
        0o640,
        "existing mode preserved"
    );
    drop(d);
}

/// `issue comment` refuses a credential-shaped body: the error names the
/// rule, never the value, and no comment file or tracker commit is written.
/// `report` is refused the same way, both as a new issue and as `--issue`.
#[test]
fn issue_comment_and_report_refuse_credential_shaped_text() {
    let s = ReportFx::new();
    let state = s.state.to_str().unwrap().to_string();
    let env = [("CADENCE_STATE_DIR", state.as_str())];
    let (ok, out) = s.cli(&["issue", "new", "Target", "--project", "product"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let comments = s.pm_dir.join("product").join(&id).join("comments");
    let count = || std::fs::read_dir(&comments).map(|d| d.count()).unwrap_or(0);
    let log_before = s.tracker_log(1);

    let tok = cad109_token("figd_", "comment", 40);
    let body = format!("Verified locally.\n{tok}\n");
    let (ok, stderr, _) = s.cli_at_env(
        &s.product_repo,
        &["issue", "comment", &id, "-m", &body],
        &env,
    );
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("rule cadence-figma-token"), "{stderr}");
    assert!(
        stderr.contains("secret_detected") || stderr.contains("refused"),
        "{stderr}"
    );
    assert!(!stderr.contains(&tok[5..]), "{stderr}");
    assert_eq!(count(), 0);
    assert_eq!(s.tracker_log(1), log_before);

    for args in [
        vec!["report", "--kind", "bug", "-m", &body],
        vec!["report", "--issue", &id, "--kind", "bug", "-m", &body],
    ] {
        let (ok, stderr, _) = s.cli_at_env(&s.product_repo, &args, &env);
        assert!(!ok, "{args:?}: {stderr}");
        assert!(stderr.contains("rule cadence-figma-token"), "{stderr}");
        assert!(!stderr.contains(&tok[5..]), "{stderr}");
    }
    assert_eq!(count(), 0);
    assert_eq!(s.tracker_log(1), log_before);

    // Clean text still goes through, and a warn-only finding rides along.
    let warn = format!(
        "config:\n  api_key = \"{}\"\n",
        cad109_token("", "warn", 24)
    );
    let (ok, stderr, (_, v)) = s.cli_at_env(
        &s.product_repo,
        &["issue", "comment", &id, "-m", &warn],
        &env,
    );
    assert!(ok, "{stderr}");
    assert_eq!(v["secret_warnings"][0]["rule"], "generic-api-key", "{v}");
    assert_eq!(count(), 1);
}

// ---- CAD-257: session gate scope, expiring acks ----

/// `cadence session start` stdout+stderr and exit code.
fn session_text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// An idle agent holding one `unknown` message — a reconcile `fail`
/// keyed `reconcile:<id>`.
fn unknown_msg_agent(alias: &str, id: &str) -> StubAgent {
    stub_agent(
        alias,
        "fake",
        "fake",
        "idle",
        60,
        (
            vec![json!({"id": id, "state": "unknown", "body": "lost turn"})],
            0,
            1,
        ),
    )
}

/// An acknowledged item warns until its expiry and fails after it —
/// the expired record is written straight into the store (no sleeps),
/// and `ack --list` keeps it, marked expired.
#[test]
fn session_start_ack_downgrades_until_expiry() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
    );
    seed_pm(&pm, &repo, &tmp.path().join("notes"));
    seed_repo(&repo);
    let _sd = stub_daemon(
        &state,
        cadence_agent::overview::BUILD_COMMIT,
        vec![unknown_msg_agent("w1", "m-unk")],
    );
    let host = clean_host(tmp.path());
    let start = || run_session_host(&state, &pm, &repo, &["session", "start"], &host, &[]);

    let out = start();
    let text = session_text(&out);
    assert_eq!(out.status.code(), Some(2), "unacked fails:\n{text}");
    assert!(text.contains("[reconcile:m-unk]"), "key printed:\n{text}");
    assert!(text.contains("cadence session ack <key>"), "hint:\n{text}");

    // A credential-shaped reason is refused by CAD-109's scan before
    // the store is written. The synthetic token is assembled at run
    // time so no credential-shaped literal is committed.
    let token = ["gh", "p_", "0123456789abcdefghij", "ABCDEFGHIJ012345"].concat();
    let reason = format!("token {token}");
    let out = run_session(
        &state,
        &pm,
        &repo,
        &[
            "session",
            "ack",
            "reconcile:m-unk",
            "--reason",
            &reason,
            "--expires",
            "1d",
        ],
    );
    let text = session_text(&out);
    assert!(!out.status.success(), "secret reason accepted:\n{text}");
    assert!(
        text.contains("credential-shaped") && !text.contains(&token),
        "{text}"
    );

    // Refusals: past the 14-day cap, in the past, no reason.
    for args in [
        vec![
            "session",
            "ack",
            "reconcile:m-unk",
            "--reason",
            "x",
            "--expires",
            "15d",
        ],
        vec![
            "session",
            "ack",
            "reconcile:m-unk",
            "--reason",
            "x",
            "--expires",
            "2020-01-01T00:00:00Z",
        ],
        vec![
            "session",
            "ack",
            "reconcile:m-unk",
            "--reason",
            " ",
            "--expires",
            "1d",
        ],
    ] {
        let out = run_session(&state, &pm, &repo, &args);
        assert!(
            !out.status.success(),
            "{args:?} accepted:\n{}",
            session_text(&out)
        );
    }
    assert!(
        !state.join("sessions/acks.json").exists(),
        "a refusal wrote the store"
    );

    let out = run_session(
        &state,
        &pm,
        &repo,
        &[
            "session",
            "ack",
            "reconcile:m-unk",
            "--reason",
            "known lost turn",
            "--expires",
            "2d",
        ],
    );
    assert!(out.status.success(), "{}", session_text(&out));
    let out = start();
    let text = session_text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "acked fail downgrades to warn:\n{text}"
    );
    assert!(
        text.contains("[reconcile:m-unk]"),
        "an ack never hides the item:\n{text}"
    );
    assert!(
        text.contains("(acknowledged until") && text.contains("known lost turn"),
        "ack reason shown:\n{text}"
    );

    // Expired: the same record with an expiry already past.
    let store = state.join("sessions/acks.json");
    let mut acks: Value = serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
    assert_eq!(acks["acks"][0]["key"], "reconcile:m-unk");
    assert!(acks["acks"][0]["actor"]
        .as_str()
        .is_some_and(|a| !a.is_empty()));
    acks["acks"][0]["expires"] = json!("2026-01-01T00:00:00Z");
    std::fs::write(&store, acks.to_string()).unwrap();
    let out = start();
    let text = session_text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "expired ack fails again:\n{text}"
    );
    assert!(
        text.contains("(acknowledgement expired 2026-01-01T00:00:00Z"),
        "{text}"
    );

    let out = run_session(&state, &pm, &repo, &["session", "ack", "--list", "--json"]);
    let list: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(list["acks"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list["acks"][0]["state"], "expired", "{list}");
}

/// Default scope is the cwd repo's project: another project's failing
/// item collapses to the `others` summary and cannot no-go the gate;
/// `--all` judges it again, and a cwd outside every project repo
/// behaves as `--all` and says so.
#[test]
fn session_start_scopes_to_cwd_project() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, other) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("other-repo"),
    );
    seed_pm(&pm, &repo, &tmp.path().join("notes"));
    seed_repo(&repo);
    std::fs::create_dir_all(pm.join("oth")).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        pm.join("oth/project.yaml"),
        format!(
            "key: oth\nprefix: OTH\nrepos:\n- path: {}\n",
            other.display()
        ),
    )
    .unwrap();
    let _sd = stub_daemon(
        &state,
        cadence_agent::overview::BUILD_COMMIT,
        vec![unknown_msg_agent("ow", "m-oth").with_cwd(&other)],
    );
    let host = clean_host(tmp.path());

    let out = run_session_host(&state, &pm, &repo, &["session", "start"], &host, &[]);
    let text = session_text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "other project's fail capped at warn:\n{text}"
    );
    assert!(text.contains("scope: project tst"), "{text}");
    assert!(text.contains("others"), "summary row:\n{text}");
    assert!(
        text.contains("oth 1 (worst fail)"),
        "per-project count:\n{text}"
    );
    assert!(
        !text.contains("[reconcile:m-oth]"),
        "collapsed, not listed:\n{text}"
    );
    // The cwd project's own findings are still judged and listed.
    assert!(text.contains("tst-88-ghost"), "{text}");

    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "start", "--all"],
        &host,
        &[],
    );
    let text = session_text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "--all judges every project:\n{text}"
    );
    assert!(text.contains("[reconcile:m-oth]"), "{text}");

    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "start", "--project", "oth", "--json"],
        &host,
        &[],
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(2), "{v}");
    assert_eq!(v["scope"]["project"], "oth");

    // Outside any project repo: fleet-wide, with the reason printed.
    let out = run_session_host(&state, &pm, tmp.path(), &["session", "start"], &host, &[]);
    let text = session_text(&out);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(text.contains("not inside a known project repo"), "{text}");
}
