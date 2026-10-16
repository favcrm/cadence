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
fn a_build_without_skill_sync_installs_its_own_bytes() {
    let fixture = Fixture::new();
    let stub = fixture.root.path().join("legacy");
    fs::write(
        &stub,
        "#!/bin/sh\nset -- \"$@\"\nlast=\nfor arg in \"$@\"; do last=\"$arg\"; done\nif [ \"$last\" = sync ]; then exit 2; fi\nmkdir -p \"$HOME/.agents/skills/legacy-stub\"\nprintf 'legacy\\n' > \"$HOME/.agents/skills/legacy-stub/WHO\"\nprintf '%s\\n' '{\"wrote\":true,\"skills\":[]}'\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let report =
        skill::invoke_activated_sync(&stub, &fixture.home, &fixture.root.path().join("state"));
    assert_eq!(report["guidance"], "current");
    assert_eq!(report["wrote"], true);
    assert_eq!(report["source_command"], "skill install");
    assert_eq!(
        fs::read(fixture.home.join(".agents/skills/legacy-stub/WHO")).unwrap(),
        b"legacy\n"
    );
    assert!(!fixture
        .home
        .join(".agents/skills/cadence/SKILL.md")
        .exists());
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
        if output.status.success() {
            let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
                panic!(
                    "{args:?} stdout={}",
                    String::from_utf8_lossy(&output.stdout)
                )
            });
            assert_eq!(body["guidance"]["wrote"], false, "{args:?}");
            assert_eq!(
                body["guidance"]["target_guidance"], "not-verified",
                "{args:?}"
            );
            assert_eq!(body["guidance"]["live_session"], "not-checked", "{args:?}");
        }
    }
}
