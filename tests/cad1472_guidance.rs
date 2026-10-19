//! CAD-1472: guidance sync is part of the active build, not a setup flag.
#![cfg(unix)]

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cadence_agent::{setup, skill};
use serde_json::Value;
use tempfile::{Builder, TempDir};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");

struct Fixture {
    root: TempDir,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Builder::new().prefix("c1472-").tempdir_in("/tmp").unwrap();
        for dir in [
            "home", "state", "xdg", "config", "cache", "tmp", "locks", "pm",
        ] {
            fs::create_dir(root.path().join(dir)).unwrap();
        }
        Self {
            home: root.path().join("home"),
            root,
        }
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.cli_env("/usr/bin:/bin", args)
    }

    fn cli_env(&self, path: &str, args: &[&str]) -> Output {
        self.cli_vars(path, &[], args)
    }

    fn cli_vars(&self, path: &str, extra: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = Command::new(BIN);
        command
            .env_clear()
            .current_dir(self.root.path())
            .arg("--state-dir")
            .arg(self.root.path().join("state"))
            .args(args)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("XDG_STATE_HOME", self.root.path().join("xdg"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("CADENCE_PM_DIR", self.root.path().join("pm"))
            .env(
                "CADENCE_SUITE_LOCK",
                self.root.path().join("locks/suite.lock"),
            )
            .stdin(Stdio::null());
        for (key, value) in extra {
            command.env(key, value);
        }
        for key in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        cadence_agent::reaper::output(&mut command).unwrap()
    }

    fn cli_json(&self, args: &[&str]) -> Value {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{args:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn age(path: &Path) -> SystemTime {
    let changed = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(changed).set_accessed(changed))
        .unwrap();
    changed
}

#[test]
fn skill_sync_repairs_managed_files_and_a_second_run_writes_nothing() {
    let fixture = Fixture::new();
    let first = fixture.cli_json(&["skill", "sync"]);
    assert_eq!(first["wrote"], true);
    assert_eq!(first["guidance"], "current");
    assert_eq!(first["live_session"], "not-checked");
    assert_eq!(first["status"]["repair"], Value::Null);
    let reference = fixture
        .home
        .join(".agents/skills/company-outputs/references/document-design.md");
    assert!(reference.is_file());
    let untouched = fixture.home.join(".agents/skills/cadence/SKILL.md");
    let old = age(&untouched);
    fs::write(&reference, "stale reference\n").unwrap();
    fs::remove_file(
        fixture
            .home
            .join(".agents/skills/company-outputs/references/workflow.md"),
    )
    .unwrap();
    let link_parent = fixture.home.join(".claude/skills");
    fs::create_dir_all(&link_parent).unwrap();
    fs::remove_file(link_parent.join("cadence")).unwrap();
    symlink("/tmp/not-the-bundle", link_parent.join("cadence")).unwrap();
    // The first sync already owns this path. Replace that link with a real
    // directory so the repair must leave a foreign provider entry alone.
    let foreign = link_parent.join("business-documents");
    fs::remove_file(&foreign).unwrap();
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep.txt"), "foreign\n").unwrap();

    let repaired = fixture.cli_json(&["skill", "sync"]);
    assert_eq!(repaired["wrote"], true);
    assert_eq!(repaired["guidance"], "needs-attention");
    assert!(repaired["attention"].as_str().unwrap().contains("foreign"));
    assert_ne!(fs::read(&reference).unwrap(), b"stale reference\n");
    assert!(fixture
        .home
        .join(".agents/skills/company-outputs/references/workflow.md")
        .is_file());
    assert_eq!(
        fs::read_link(link_parent.join("cadence")).unwrap(),
        fixture.home.join(".agents/skills/cadence")
    );
    assert_eq!(fs::read(foreign.join("keep.txt")).unwrap(), b"foreign\n");
    assert_eq!(fs::metadata(&untouched).unwrap().modified().unwrap(), old);

    let unchanged_mtime = fs::metadata(&reference).unwrap().modified().unwrap();
    let second = fixture.cli_json(&["skill", "sync"]);
    assert_eq!(second["wrote"], false);
    assert_eq!(
        fs::metadata(&reference).unwrap().modified().unwrap(),
        unchanged_mtime
    );
    assert_eq!(fs::metadata(&untouched).unwrap().modified().unwrap(), old);

    let before = fs::metadata(&untouched).unwrap().modified().unwrap();
    let status = fixture.cli_json(&["skill", "status"]);
    assert_eq!(status["live_session"], "not-checked");
    assert_eq!(status["guidance"], "needs-attention");
    assert_eq!(status["file_match"], "current");
    assert_eq!(status["provider_discovery"], "needs-attention");
    assert!(status["repair"].is_null());
    assert!(status["attention"].as_str().unwrap().contains("foreign"));
    assert_eq!(
        fs::metadata(&untouched).unwrap().modified().unwrap(),
        before
    );
}

#[test]
fn activated_binary_writes_its_own_report_and_a_missing_binary_writes_nothing() {
    let fixture = Fixture::new();
    let stub = fixture.root.path().join("activated");
    fs::write(
        &stub,
        "#!/bin/sh\nmkdir -p \"$HOME/.agents/skills/activated-stub\"\nprintf 'activated\\n' > \"$HOME/.agents/skills/activated-stub/WHO\"\nprintf '%s\\n' '{\"wrote\":true,\"guidance\":\"current\",\"live_session\":\"not-checked\"}'\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let report =
        skill::invoke_activated_sync(&stub, &fixture.home, &fixture.root.path().join("state"));
    assert_eq!(report["guidance"], "current");
    assert_eq!(report["wrote"], true);
    assert_eq!(
        fs::read(fixture.home.join(".agents/skills/activated-stub/WHO")).unwrap(),
        b"activated\n"
    );
    assert!(
        !fixture
            .home
            .join(".agents/skills/cadence/SKILL.md")
            .exists(),
        "the parent process must not write its own embedded guidance"
    );

    let missing = skill::invoke_activated_sync(
        &fixture.root.path().join("no-such-binary"),
        &fixture.home,
        &fixture.root.path().join("state"),
    );
    assert_eq!(missing["guidance"], "not-checked");
    assert_eq!(missing["wrote"], false);
    assert!(!fixture
        .home
        .join(".agents/skills/cadence/SKILL.md")
        .exists());
}

#[test]
fn a_build_without_skill_sync_takes_its_bytes_non_force() {
    let fixture = Fixture::new();
    let stub = fixture.root.path().join("legacy");
    // A build that predates `skill sync`: it refuses `sync`, and its
    // `install` rewrites every file it ships, as the real older releases do.
    fs::write(
        &stub,
        "#!/bin/sh\nfor arg in \"$@\"; do last=\"$arg\"; done\nif [ \"$last\" = sync ]; then exit 2; fi\n\
         mkdir -p \"$HOME/.agents/skills/cadence\" \"$HOME/.agents/skills/business-documents\"\n\
         printf 'legacy skill\\n' > \"$HOME/.agents/skills/cadence/SKILL.md\"\n\
         printf 'legacy operator\\n' > \"$HOME/.agents/skills/business-documents/OPERATOR.md\"\n\
         printf '%s\\n' '{\"wrote\":true,\"skills\":[]}'\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let state = fixture.root.path().join("state");
    let reference = fixture.home.join(".agents/skills/cadence/SKILL.md");
    let other = fixture
        .home
        .join(".agents/skills/business-documents/OPERATOR.md");

    let report = skill::invoke_activated_sync(&stub, &fixture.home, &state);
    assert_eq!(report["guidance"], "current");
    assert_eq!(report["wrote"], true);
    assert_eq!(report["source_command"], "skill install");
    assert_eq!(report["staged"], true);
    assert_eq!(fs::read(&reference).unwrap(), b"legacy skill\n");
    assert_eq!(fs::read(&other).unwrap(), b"legacy operator\n");
    // The bytes are the activated build's, never this process's.
    assert_ne!(fs::read(&reference).unwrap(), skill::SKILL_MD.as_bytes());
    assert!(
        !fixture
            .home
            .join(".agents/skills/company-outputs/SKILL.md")
            .exists(),
        "a bundle the activated build never shipped is not invented here"
    );

    // The compatibility path must not rewrite a file whose bytes already
    // match: age the timestamps, make one file stale, and only that one is
    // written. This is the repeat-run promise for a legacy active build.
    let aged = fs::metadata(&reference).unwrap().modified().unwrap();
    fs::write(&other, "stale operator\n").unwrap();
    let stale_at = fs::metadata(&other).unwrap().modified().unwrap();
    let second = skill::invoke_activated_sync(&stub, &fixture.home, &state);
    assert_eq!(second["guidance"], "current");
    assert_eq!(second["wrote"], true);
    assert_eq!(fs::read(&other).unwrap(), b"legacy operator\n");
    assert!(fs::metadata(&other).unwrap().modified().unwrap() != stale_at);
    assert_eq!(
        fs::metadata(&reference).unwrap().modified().unwrap(),
        aged,
        "a matching file keeps its timestamp"
    );

    // A third run with nothing stale writes nothing at all.
    let quiet = fs::metadata(&reference).unwrap().modified().unwrap();
    let third = skill::invoke_activated_sync(&stub, &fixture.home, &state);
    assert_eq!(third["wrote"], false);
    assert_eq!(third["guidance"], "current");
    assert_eq!(fs::metadata(&reference).unwrap().modified().unwrap(), quiet);
}

#[test]
fn fresh_setup_reports_scope_without_touching_a_non_git_cwd_or_snapshot() {
    let fixture = Fixture::new();
    let snapshot = fixture.root.path().join("skills/company-outputs");
    fs::create_dir_all(&snapshot).unwrap();
    fs::write(snapshot.join("SKILL.md"), "snapshot\n").unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    struct Stop<'a>(&'a Fixture);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = self.0.cli(&["ui", "stop"]);
            let _ = self.0.cli(&["daemon", "stop"]);
        }
    }
    let _stop = Stop(&fixture);
    let output = fixture.cli(&["setup", "--json", "--port", &port.to_string()]);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.root.path().join(".git").exists());
    assert_eq!(fs::read(snapshot.join("SKILL.md")).unwrap(), b"snapshot\n");
    assert!(!fixture
        .root
        .path()
        .join("config/cadence/orgs.json")
        .exists());
    assert!(!fixture
        .root
        .path()
        .join("config/cadence/orgs.lock")
        .exists());
    let lines: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let skill = lines.iter().find(|row| row["check"] == "skill").unwrap();
    assert_eq!(skill["status"], "created");
    assert!(fixture
        .home
        .join(".agents/skills/company-outputs/references/document-design.md")
        .is_file());
    assert!(fixture
        .home
        .join(".agents/skills/cadence/SKILL.md")
        .is_file());
    assert!(fixture
        .home
        .join(".agents/skills/business-documents/SKILL.md")
        .is_file());
    let pretty = fixture.cli_vars(
        "/usr/bin:/bin",
        &[("CADENCE_JSON", "pretty")],
        &["setup", "--json", "--port", &port.to_string()],
    );
    assert!(
        pretty.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&pretty.stdout),
        String::from_utf8_lossy(&pretty.stderr)
    );
    let pretty_text = String::from_utf8_lossy(&pretty.stdout);
    let pretty_lines: Vec<Value> = pretty_text
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("setup --json must stay one object per line ({error}): {line}")
            })
        })
        .collect();
    assert!(pretty_lines.iter().any(|row| row["check"] == "scope"));

    let scope = lines.iter().find(|row| row["check"] == "scope").unwrap();
    assert_eq!(scope["project"]["resolved"], false);
    assert!(scope["project"]["detail"]
        .as_str()
        .unwrap()
        .contains("not a git checkout"));
    assert_eq!(scope["org"]["changed"], false);
    assert_eq!(scope["file_match"], "current");
    assert_eq!(scope["provider_discovery"], "current");
    assert_eq!(scope["guidance"], "current");
    assert_eq!(scope["live_session"], "not-checked");
    assert_eq!(scope["unmanaged_snapshots"][0]["active"], false);
    assert_ne!(scope["state_dir"], scope["tracker"]);

    let reference = fixture
        .home
        .join(".agents/skills/company-outputs/references/workflow.md");
    fs::write(&reference, "leave this\n").unwrap();
    let old = age(&reference);
    let context = setup::Ctx {
        state_dir: fixture.root.path().join("state"),
        pm_dir: fixture.root.path().join("pm"),
        home: fixture.home.clone(),
        port: Some(port),
        sandbox: None,
        env: |_| None,
        probe_timeout: std::time::Duration::from_secs(1),
        verbs: Vec::new(),
    };
    let checks = [setup::checks()
        .into_iter()
        .find(|check| check.name == "skill")
        .unwrap()];
    let detected = setup::detect_checks(&context, &checks);
    assert_ne!(detected[0].status, setup::Status::Created);
    assert_eq!(fs::read(&reference).unwrap(), b"leave this\n");
    assert_eq!(fs::metadata(&reference).unwrap().modified().unwrap(), old);
    let observed = checks[0].run(&context, &[]);
    assert!(matches!(observed.status, setup::Status::Unknown));
    let fix = observed.fix.unwrap();
    assert!(fix.contains("skill install"), "{fix}");
    assert!(!fix.contains("refresh-skills"), "{fix}");
    assert_eq!(fs::read(&reference).unwrap(), b"leave this\n");
}

#[test]
fn update_check_and_status_leave_guidance_bytes_untouched() {
    let fixture = Fixture::new();
    assert_eq!(fixture.cli_json(&["skill", "sync"])["wrote"], true);
    let reference = fixture
        .home
        .join(".agents/skills/company-outputs/references/workflow.md");
    let bytes = fs::read(&reference).unwrap();
    let old = age(&reference);
    let bin = fixture.root.path().join("gh-bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\necho 'not authenticated' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let link = fixture.root.path().join("link/cadence");
    let releases = fixture.root.path().join("releases");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    fs::create_dir(&releases).unwrap();
    let path = bin.to_string_lossy().to_string();
    for args in [
        vec![
            "update",
            "--json",
            "status",
            "--link",
            link.to_str().unwrap(),
            "--releases-dir",
            releases.to_str().unwrap(),
        ],
        vec![
            "update",
            "--check",
            "--json",
            "--link",
            link.to_str().unwrap(),
            "--releases-dir",
            releases.to_str().unwrap(),
        ],
    ] {
        let output = fixture.cli_env(&path, &args.to_vec());
        assert_eq!(fs::read(&reference).unwrap(), bytes, "{args:?}");
        assert_eq!(
            fs::metadata(&reference).unwrap().modified().unwrap(),
            old,
            "{args:?} stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() {
            let body: Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|_| panic!("{args:?} stdout={stdout}"));
            assert_eq!(body["guidance"]["wrote"], false, "{args:?}");
            assert_eq!(
                body["guidance"]["target_guidance"], "not-verified",
                "{args:?}"
            );
            assert_eq!(body["guidance"]["live_session"], "not-checked", "{args:?}");
        } else {
            // The owned catalogue is the default source, so `--check` can
            // refuse before `gh` login (no `tar`, no reachable catalogue).
            // That refusal is not a guidance write; the byte checks above
            // are the contract.
            let rejected = serde_json::from_slice::<Value>(&output.stderr)
                .ok()
                .is_some_and(|body| body["kind"] == "rejected");
            assert!(
                rejected
                    || stderr.contains("not authenticated")
                    || stderr.contains("operator")
                    || stderr.contains("gh"),
                "{args:?} stderr={stderr}"
            );
            assert!(
                !stdout.contains("guidance: current"),
                "{args:?} claimed current guidance: {stdout}"
            );
        }
    }
}

/// Disposable already-current `cadence update`: the real CLI, a real
/// daemon on this build, and the release copy's own bytes. `gh` answers
/// only the candidate lookup, because an unmerged build is not an
/// attested production install. No production state, link, or home.
///
/// The repo is not `favcrm/cadence`. That name selects the owned
/// catalogue, which proves a published package before it will call the
/// build current. This fixture stays on the live candidate route.
#[cfg(feature = "test-seam")]
#[test]
fn already_current_update_repairs_guidance_without_restarting_the_daemon() {
    let fixture = Fixture::new();
    let path = format!(
        "{}:/usr/bin:/bin",
        fixture.root.path().join("gh-bin").display()
    );
    fs::create_dir(fixture.root.path().join("gh-bin")).unwrap();
    let gh = fixture.root.path().join("gh-bin/gh");
    fs::write(
        &gh,
        r#"#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
sha = os.environ["CANDIDATE_SHA"]
def flag(name):
    if name in args:
        return args[args.index(name) + 1]
    return None
if args[:2] == ["auth", "status"]:
    sys.exit(0)
if args[:2] == ["run", "list"] and "staging.yml" in args:
    json.dump([{
        "databaseId": 4242, "attempt": 1, "headSha": sha, "headBranch": "main",
        "event": "workflow_dispatch", "status": "completed", "conclusion": "success",
    }], sys.stdout)
    sys.exit(0)
if args[:2] == ["run", "view"]:
    json.dump({"jobs": [
        {"name": "stage", "status": "completed", "conclusion": "success"},
        {"name": "promote", "status": "completed", "conclusion": "success"},
    ]}, sys.stdout)
    sys.exit(0)
if args[:2] == ["run", "download"]:
    dest = pathlib.Path(flag("--dir"))
    dest.mkdir(parents=True, exist_ok=True)
    (dest / "production-candidate.json").write_text(json.dumps({
        "schema": 1, "source_sha": sha, "ci_run_id": 4343, "ci_run_attempt": 1,
        "sha256": "ab" * 32, "promotion_run_id": 4242, "promotion_run_attempt": 1,
    }))
    sys.exit(0)
if args[:1] == ["api"] and args[1].endswith("/actions/runs/4343"):
    json.dump({
        "id": 4343, "run_attempt": 1, "head_sha": sha, "head_branch": "main",
        "event": "push", "path": ".github/workflows/ci.yml",
        "status": "completed", "conclusion": "success",
        "repository": {"full_name": "favcrm/cadence-unmerged"},
        "head_repository": {"full_name": "favcrm/cadence-unmerged"},
    }, sys.stdout)
    sys.exit(0)
sys.stderr.write("unexpected gh: %s\n" % " ".join(args))
sys.exit(1)
"#,
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();

    let version = fixture.cli_env(&path, &["--version"]);
    assert!(
        version.status.success(),
        "{}",
        String::from_utf8_lossy(&version.stderr)
    );
    let sha = String::from_utf8_lossy(&version.stdout)
        .trim()
        .rsplit_once('+')
        .unwrap()
        .1
        .to_string();
    assert_eq!(sha.len(), 40, "{sha}");

    let releases = fixture.root.path().join("releases");
    let release_bin = releases.join(&sha).join("cadence");
    fs::create_dir_all(release_bin.parent().unwrap()).unwrap();
    fs::copy(BIN, &release_bin).unwrap();
    fs::set_permissions(&release_bin, fs::Permissions::from_mode(0o755)).unwrap();
    let link = fixture.root.path().join("link/cadence");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&release_bin, &link).unwrap();

    // The seam refuses a state dir that is not under the process temp
    // root. This fixture's default TMPDIR is a sibling of `state`.
    let temp_root = fixture
        .root
        .path()
        .to_str()
        .expect("fixture path is utf-8")
        .to_string();
    let seam = [
        ("CADENCE_TEST_SEAM", "1"),
        ("CANDIDATE_SHA", sha.as_str()),
        ("TMPDIR", temp_root.as_str()),
    ];
    let started = fixture.cli_vars(&path, &seam, &["daemon", "start"]);
    assert!(
        started.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    struct Stop<'a>(&'a Fixture, String);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = self.0.cli_env(&self.1, &["daemon", "stop"]);
        }
    }
    let _stop = Stop(&fixture, path.clone());
    let mut daemon_up = false;
    for _ in 0..50 {
        let status = fixture.cli_vars(&path, &seam, &["daemon", "status"]);
        if status.status.success() {
            daemon_up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(daemon_up, "daemon did not answer");

    let reference = fixture
        .home
        .join(".agents/skills/company-outputs/references/document-design.md");
    let original = fs::read(&reference).unwrap();
    fs::write(&reference, "stale reference\n").unwrap();
    let missing = fixture
        .home
        .join(".agents/skills/company-outputs/references/workflow.md");
    fs::remove_file(&missing).unwrap();
    let link_parent = fixture.home.join(".claude/skills");
    fs::remove_file(link_parent.join("cadence")).unwrap();
    symlink("/tmp/not-the-bundle", link_parent.join("cadence")).unwrap();

    let update_env = [
        ("CADENCE_TEST_SEAM", "1"),
        ("CADENCE_TEST_AS", "operator"),
        ("CANDIDATE_SHA", sha.as_str()),
        ("TMPDIR", temp_root.as_str()),
    ];
    let args = [
        "update",
        "--json",
        "--link",
        link.to_str().unwrap(),
        "--releases-dir",
        releases.to_str().unwrap(),
        "--repo",
        "favcrm/cadence-unmerged",
    ];
    let output = fixture.cli_vars(&path, &update_env, &args);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout} stderr={stderr}");
    let body: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| serde_json::from_str(stdout.lines().last().unwrap()).unwrap());
    assert_eq!(body["check"]["up_to_date"], true, "{body}");
    assert!(body["restart"].is_null(), "{body}");
    assert_eq!(body["guidance"]["wrote"], true, "{body}");
    assert_eq!(body["guidance"]["source"], "activated-binary", "{body}");
    assert_eq!(body["guidance"]["activated_sha"], sha, "{body}");
    assert_eq!(
        body["guidance"]["daemon_restarted_for_skills"], false,
        "{body}"
    );
    assert_eq!(body["guidance"]["live_session"], "not-checked", "{body}");
    assert_eq!(fs::read(&reference).unwrap(), original);
    assert!(missing.is_file());
    assert_eq!(
        fs::read_link(link_parent.join("cadence")).unwrap(),
        fixture.home.join(".agents/skills/cadence")
    );

    let unchanged = fs::metadata(&reference).unwrap().modified().unwrap();
    let second = fixture.cli_vars(&path, &update_env, &args);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let again: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(again["guidance"]["wrote"], false, "{again}");
    assert!(again["restart"].is_null(), "{again}");
    assert_eq!(
        fs::metadata(&reference).unwrap().modified().unwrap(),
        unchanged
    );

    fs::write(&reference, "check must not repair\n").unwrap();
    let link_arg = link.to_str().unwrap();
    let releases_arg = releases.to_str().unwrap();
    let read_only_cases: [&[&str]; 2] = [
        &[
            "update",
            "--check",
            "--json",
            "--link",
            link_arg,
            "--releases-dir",
            releases_arg,
            "--repo",
            "favcrm/cadence-unmerged",
        ],
        &[
            "update",
            "--json",
            "status",
            "--link",
            link_arg,
            "--releases-dir",
            releases_arg,
        ],
    ];
    for read_only in read_only_cases {
        let output = fixture.cli_vars(&path, &update_env, read_only);
        assert!(
            output.status.success(),
            "{read_only:?} stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(body["guidance"]["wrote"], false, "{read_only:?} {body}");
        assert_eq!(fs::read(&reference).unwrap(), b"check must not repair\n");
    }

    fs::set_permissions(&release_bin, fs::Permissions::from_mode(0o000)).unwrap();
    let unavailable = fixture.cli_vars(&path, &update_env, &args);
    fs::set_permissions(&release_bin, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        unavailable.status.success(),
        "a guidance miss must not fail the already-current binary result: {}",
        String::from_utf8_lossy(&unavailable.stderr)
    );
    let unavailable_body: Value = serde_json::from_slice(&unavailable.stdout).unwrap();
    assert_eq!(unavailable_body["check"]["up_to_date"], true);
    assert_eq!(
        unavailable_body["guidance"]["wrote"], false,
        "{unavailable_body}"
    );
    assert_eq!(
        unavailable_body["guidance"]["guidance"], "not-checked",
        "{unavailable_body}"
    );
    assert_eq!(fs::read(&reference).unwrap(), b"check must not repair\n");

    // Overwriting an existing file does not need directory write
    // permission. The file itself has to be unwritable.
    let _restore_reference = Mode(&reference, 0o644);
    fs::set_permissions(&reference, fs::Permissions::from_mode(0o444)).unwrap();
    let write_failed = fixture.cli_vars(&path, &update_env, &args);
    fs::set_permissions(&reference, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        write_failed.status.success(),
        "a guidance write failure must not fail the binary result: {}",
        String::from_utf8_lossy(&write_failed.stderr)
    );
    let write_body: Value = serde_json::from_slice(&write_failed.stdout).unwrap();
    assert_eq!(write_body["check"]["up_to_date"], true, "{write_body}");
    assert!(write_body["restart"].is_null(), "{write_body}");
    assert_eq!(write_body["guidance"]["wrote"], false, "{write_body}");
    assert_eq!(
        write_body["guidance"]["guidance"], "needs-attention",
        "{write_body}"
    );
    let write_error = write_body["guidance"]["error"].as_str().unwrap_or("");
    assert!(
        write_error.contains("write"),
        "write failure was not reported: {write_body}"
    );
    assert_eq!(fs::read(&reference).unwrap(), b"check must not repair\n");

    let repaired = fixture.cli_vars(&path, &update_env, &args);
    assert!(
        repaired.status.success(),
        "{}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    assert_eq!(fs::read(&reference).unwrap(), original);

    // A repairable provider link whose parent cannot be updated stays
    // wrong, and the report names the failure instead of claiming current.
    let provider_link = link_parent.join("cadence");
    fs::remove_file(&provider_link).unwrap();
    symlink("/tmp/not-the-bundle", &provider_link).unwrap();
    let _restore_link_parent = Mode(&link_parent, 0o755);
    fs::set_permissions(&link_parent, fs::Permissions::from_mode(0o555)).unwrap();
    let link_failed = fixture.cli_vars(&path, &update_env, &args);
    fs::set_permissions(&link_parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        link_failed.status.success(),
        "a guidance link failure must not fail the binary result: {}",
        String::from_utf8_lossy(&link_failed.stderr)
    );
    let link_body: Value = serde_json::from_slice(&link_failed.stdout).unwrap();
    assert_eq!(link_body["check"]["up_to_date"], true, "{link_body}");
    assert!(link_body["restart"].is_null(), "{link_body}");
    assert_eq!(link_body["guidance"]["wrote"], false, "{link_body}");
    assert_eq!(
        link_body["guidance"]["guidance"], "needs-attention",
        "{link_body}"
    );
    let link_error = link_body["guidance"]["error"].as_str().unwrap_or("");
    assert!(
        link_error.contains("replace") || link_error.contains("link"),
        "link failure was not reported: {link_body}"
    );
    assert_eq!(
        fs::read_link(&provider_link).unwrap(),
        Path::new("/tmp/not-the-bundle")
    );

    fs::write(&reference, "rollback must not repair\n").unwrap();
    let rollback = fixture.cli_vars(
        &path,
        &update_env,
        &[
            "update",
            "--rollback",
            "--json",
            "--link",
            link.to_str().unwrap(),
            "--releases-dir",
            releases.to_str().unwrap(),
        ],
    );
    assert!(
        !rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stdout)
    );
    let rollback_err = String::from_utf8_lossy(&rollback.stderr);
    assert!(
        rollback_err.contains("no previous release"),
        "{rollback_err}"
    );
    assert_eq!(fs::read(&reference).unwrap(), b"rollback must not repair\n");
    assert_eq!(fs::read_link(&link).unwrap(), release_bin);
}

/// Restores a path mode if an assertion fails before the test does.
#[cfg(feature = "test-seam")]
struct Mode<'a>(&'a Path, u32);

#[cfg(feature = "test-seam")]
impl Drop for Mode<'_> {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(self.1));
    }
}

/// The already-current update again, but its activated release is a build
/// that predates `skill sync`: it refuses `sync` and its `install` rewrites
/// every file it ships, exactly like the real older releases. The
/// compatibility path must still keep the non-force promise at update
/// level — only the stale file is written, a matching file keeps its
/// timestamp, and a second update writes nothing at all.
#[cfg(feature = "test-seam")]
#[test]
fn already_current_update_with_a_pre_sync_release_keeps_the_non_force_promise() {
    let fixture = Fixture::new();
    let path = format!(
        "{}:/usr/bin:/bin",
        fixture.root.path().join("gh-bin").display()
    );
    fs::create_dir(fixture.root.path().join("gh-bin")).unwrap();
    let gh = fixture.root.path().join("gh-bin/gh");
    fs::write(
        &gh,
        "#!/usr/bin/env python3\n\
         import json, os, pathlib, sys\n\
         a = sys.argv[1:]\n\
         sha = os.environ[\"CANDIDATE_SHA\"]\n\
         if a[:2] == [\"auth\", \"status\"]: sys.exit(0)\n\
         if a[:2] == [\"run\", \"list\"]:\n\
         \x20   print(json.dumps([{\"databaseId\": 4242, \"attempt\": 1, \"headSha\": sha,\n\
         \x20       \"headBranch\": \"main\", \"event\": \"workflow_dispatch\",\n\
         \x20       \"status\": \"completed\", \"conclusion\": \"success\"}])); sys.exit(0)\n\
         if a[:2] == [\"run\", \"view\"]:\n\
         \x20   print(json.dumps({\"jobs\": [\n\
         \x20       {\"name\": \"stage\", \"status\": \"completed\", \"conclusion\": \"success\"},\n\
         \x20       {\"name\": \"promote\", \"status\": \"completed\", \"conclusion\": \"success\"}]})); sys.exit(0)\n\
         if a[:2] == [\"run\", \"download\"]:\n\
         \x20   d = pathlib.Path(a[a.index(\"--dir\") + 1]); d.mkdir(parents=True, exist_ok=True)\n\
         \x20   (d / \"production-candidate.json\").write_text(json.dumps({\"schema\": 1,\n\
         \x20       \"source_sha\": sha, \"ci_run_id\": 4343, \"ci_run_attempt\": 1,\n\
         \x20       \"sha256\": \"ab\" * 32, \"promotion_run_id\": 4242, \"promotion_run_attempt\": 1}))\n\
         \x20   sys.exit(0)\n\
         if a[:1] == [\"api\"]:\n\
         \x20   print(json.dumps({\"id\": 4343, \"run_attempt\": 1, \"head_sha\": sha,\n\
         \x20       \"head_branch\": \"main\", \"event\": \"push\", \"path\": \".github/workflows/ci.yml\",\n\
         \x20       \"status\": \"completed\", \"conclusion\": \"success\",\n\
         \x20       \"repository\": {\"full_name\": \"favcrm/cadence-unmerged\"},\n\
         \x20       \"head_repository\": {\"full_name\": \"favcrm/cadence-unmerged\"}})); sys.exit(0)\n\
         sys.stderr.write(\"unexpected gh: %s\\n\" % \" \".join(a)); sys.exit(1)\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();

    let version = fixture.cli_env(&path, &["--version"]);
    assert!(version.status.success());
    let sha = String::from_utf8_lossy(&version.stdout)
        .trim()
        .rsplit_once('+')
        .unwrap()
        .1
        .to_string();

    let releases = fixture.root.path().join("releases");
    let release_bin = releases.join(&sha).join("cadence");
    fs::create_dir_all(release_bin.parent().unwrap()).unwrap();
    fs::write(
        &release_bin,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do last=\"$arg\"; done\n\
             if [ \"$last\" = sync ]; then exit 2; fi\nexec {BIN} \"$@\"\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&release_bin, fs::Permissions::from_mode(0o755)).unwrap();
    let link = fixture.root.path().join("link/cadence");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&release_bin, &link).unwrap();

    let temp_root = fixture
        .root
        .path()
        .to_str()
        .expect("fixture path is utf-8")
        .to_string();
    let seam = [
        ("CADENCE_TEST_SEAM", "1"),
        ("CANDIDATE_SHA", sha.as_str()),
        ("TMPDIR", temp_root.as_str()),
    ];
    let started = fixture.cli_vars(&path, &seam, &["daemon", "start"]);
    assert!(
        started.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    struct Stop<'a>(&'a Fixture, String);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = self.0.cli_env(&self.1, &["daemon", "stop"]);
        }
    }
    let _stop = Stop(&fixture, path.clone());
    let mut daemon_up = false;
    for _ in 0..50 {
        if fixture
            .cli_vars(&path, &seam, &["daemon", "status"])
            .status
            .success()
        {
            daemon_up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(daemon_up, "daemon did not answer");

    let reference = fixture
        .home
        .join(".agents/skills/company-outputs/references/document-design.md");
    let stale = fixture.home.join(".agents/skills/cadence/SKILL.md");
    let untouched = fixture
        .home
        .join(".agents/skills/business-documents/SKILL.md");
    fs::remove_file(&reference).unwrap();
    fs::write(&stale, "stale skill\n").unwrap();
    let quiet = age(&untouched);

    let update_env = [
        ("CADENCE_TEST_SEAM", "1"),
        ("CADENCE_TEST_AS", "operator"),
        ("CANDIDATE_SHA", sha.as_str()),
        ("TMPDIR", temp_root.as_str()),
    ];
    let args = [
        "update",
        "--json",
        "--link",
        link.to_str().unwrap(),
        "--releases-dir",
        releases.to_str().unwrap(),
        "--repo",
        "favcrm/cadence-unmerged",
    ];
    let output = fixture.cli_vars(&path, &update_env, &args);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["check"]["up_to_date"], true, "{body}");
    assert_eq!(body["guidance"]["source"], "activated-binary", "{body}");
    assert_eq!(body["guidance"]["activated_sha"], sha, "{body}");
    assert_eq!(
        body["guidance"]["source_command"], "skill install",
        "{body}"
    );
    assert_eq!(body["guidance"]["staged"], true, "{body}");
    assert_eq!(body["guidance"]["wrote"], true, "{body}");
    assert_eq!(body["guidance"]["guidance"], "current", "{body}");
    assert!(reference.is_file(), "the missing reference is restored");
    assert_eq!(fs::read(&stale).unwrap(), skill::SKILL_MD.as_bytes());
    assert_eq!(
        fs::metadata(&untouched).unwrap().modified().unwrap(),
        quiet,
        "a matching file keeps its timestamp through the compatibility path"
    );

    // Nothing is stale now: a second update writes nothing at all.
    let settled = fs::metadata(&stale).unwrap().modified().unwrap();
    let second = fixture.cli_vars(&path, &update_env, &args);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let again: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(again["guidance"]["wrote"], false, "{again}");
    assert_eq!(fs::metadata(&stale).unwrap().modified().unwrap(), settled);
    assert_eq!(fs::metadata(&untouched).unwrap().modified().unwrap(), quiet);
}
