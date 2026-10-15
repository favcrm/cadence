//! CAD-1454: bundled guidance installation, provider discovery and setup policy.
#![cfg(unix)]

use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cadence_agent::{setup, skill};
use serde_json::Value;
use tempfile::{Builder, TempDir};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");
const PROVIDERS: [&str; 3] = ["claude", "cursor", "copilot"];

struct Fixture {
    root: TempDir,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Builder::new().prefix("c1454-").tempdir_in("/tmp").unwrap();
        for dir in ["home", "state", "xdg", "config", "cache", "tmp", "locks"] {
            fs::create_dir(root.path().join(dir)).unwrap();
        }
        let home = root.path().join("home");
        Self { root, home }
    }

    fn canonical(&self, name: &str) -> PathBuf {
        self.home.join(".agents/skills").join(name)
    }

    fn provider(&self, provider: &str, name: &str) -> PathBuf {
        self.home.join(format!(".{provider}/skills")).join(name)
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut command = Command::new(BIN);
        command
            .env_clear()
            .current_dir(self.root.path())
            .arg("--state-dir")
            .arg(self.root.path().join("state"))
            .args(args)
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("XDG_STATE_HOME", self.root.path().join("xdg"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("TMPDIR", self.root.path().join("tmp"))
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

    fn setup_context(&self) -> setup::Ctx {
        setup::Ctx {
            state_dir: self.root.path().join("state"),
            pm_dir: self.root.path().join("pm"),
            home: self.home.clone(),
            port: None,
            sandbox: None,
            env: |_| None,
            probe_timeout: Duration::from_millis(10),
            verbs: vec!["skill".into()],
        }
    }
}

fn named_skill<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["skills"]
        .as_array()
        .expect("per-skill report")
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("missing {name} in {report}"))
}

fn reported_file<'a>(report: &'a Value, path: &Path) -> &'a Value {
    report["skills"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["files"].as_array().unwrap())
        .find(|entry| entry["path"].as_str() == path.to_str())
        .unwrap_or_else(|| panic!("missing {} in {report}", path.display()))
}

fn write_file(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Give rewrites a deterministic observable timestamp without waiting for a clock tick.
fn age_file(path: &Path) -> SystemTime {
    let old = UNIX_EPOCH + Duration::from_secs(946_684_800);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(old))
        .unwrap();
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), old);
    old
}

fn assert_home_empty(fixture: &Fixture) {
    assert!(fs::read_dir(&fixture.home).unwrap().next().is_none());
}

#[test]
fn canonical_file_symlinks_are_rejected_without_rewriting_external_targets() {
    for force in [false, true] {
        let fixture = Fixture::new();
        let outside = fixture.root.path().join("private-quotation.md");
        let private_content = b"synthetic private quotation: preserve exactly\n";
        write_file(&outside, private_content);
        let link = fixture
            .canonical("company-outputs")
            .join("references/workflow.md");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let result = skill::sync(&fixture.home, force, Some("company-outputs"));

        assert!(
            fs::read(&outside).unwrap() == private_content,
            "private target was overwritten (force={force})"
        );
        assert!(result.is_err(), "canonical symlink must be refused");
        assert_eq!(fs::read_link(&link).unwrap(), outside);
    }
}

#[test]
fn status_detects_missing_and_stale_references_and_sync_repairs_without_rewriting_matches() {
    let fixture = Fixture::new();
    fixture.cli_json(&["skill", "install"]);
    let healthy = fixture.cli_json(&["skill", "status"]);
    assert_eq!(healthy["installed"], true);
    assert_eq!(healthy["content_match"], true);

    // Stamp every installed guidance file, including the protocol and dependency.
    let aged: Vec<_> = healthy["skills"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["files"].as_array().unwrap())
        .map(|entry| {
            let path = PathBuf::from(entry["path"].as_str().unwrap());
            let old = age_file(&path);
            (path, old)
        })
        .collect();
    let unchanged = skill::sync(&fixture.home, false, None).unwrap();
    assert_eq!(unchanged["wrote"], false);
    for (path, old) in &aged {
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), *old);
    }

    let missing = fixture
        .canonical("company-outputs")
        .join("references/workflow.md");
    let stale = fixture
        .canonical("company-outputs")
        .join("references/quotation.md");
    let missing_before = fs::read(&missing).unwrap();
    let stale_before = fs::read(&stale).unwrap();
    fs::remove_file(&missing).unwrap();
    fs::write(&stale, b"outdated local reference\n").unwrap();
    let damaged = skill::status(&fixture.home);
    assert_eq!(damaged["installed"], false);
    assert_eq!(damaged["has_files"], true);
    assert_eq!(damaged["content_match"], false);
    assert_eq!(reported_file(&damaged, &missing)["installed"], false);
    assert_eq!(reported_file(&damaged, &missing)["content_match"], false);
    assert_eq!(reported_file(&damaged, &stale)["installed"], true);
    assert_eq!(reported_file(&damaged, &stale)["content_match"], false);

    let repaired = skill::sync(&fixture.home, false, Some("company-outputs")).unwrap();
    assert_eq!(repaired["wrote"], true);
    assert_eq!(fs::read(&missing).unwrap(), missing_before);
    assert_eq!(fs::read(&stale).unwrap(), stale_before);
    assert_eq!(skill::status(&fixture.home)["content_match"], true);
    for (path, old) in aged {
        if path != missing && path != stale {
            assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), old);
        }
    }
}

#[test]
fn foreign_provider_entries_survive_while_wrong_and_missing_links_are_repaired() {
    let fixture = Fixture::new();
    let foreign_file = fixture.provider("claude", "company-outputs");
    let foreign_dir = fixture.provider("cursor", "company-outputs");
    let foreign_child = foreign_dir.join("owned.bin");
    write_file(&foreign_file, b"foreign file\0\xff");
    write_file(&foreign_child, b"foreign directory contents\0\xfe");
    let file_mtime = age_file(&foreign_file);
    let child_mtime = age_file(&foreign_child);
    let managed_link = fixture.provider("copilot", "company-outputs");
    fs::create_dir_all(managed_link.parent().unwrap()).unwrap();
    let wrong_target = fixture.root.path().join("unrelated-target");
    write_file(&wrong_target.join("keep.bin"), b"not a bundled skill");
    std::os::unix::fs::symlink(&wrong_target, &managed_link).unwrap();
    let unrelated = fixture.provider("copilot", "unrelated.md");
    write_file(&unrelated, b"unrelated provider file");
    let extra = fixture.canonical("company-outputs").join("local-notes.bin");
    write_file(&extra, b"unlisted canonical file");
    let unrelated_mtime = age_file(&unrelated);
    let extra_mtime = age_file(&extra);

    let report = skill::sync(&fixture.home, false, Some("company-outputs")).unwrap();
    assert_eq!(
        named_skill(&report, "company-outputs")["skipped"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fs::read_link(&managed_link).unwrap(),
        fixture.canonical("company-outputs")
    );
    let status = skill::status(&fixture.home);
    let links = &named_skill(&status, "company-outputs")["links"];
    assert_eq!(links["claude"], "foreign");
    assert_eq!(links["cursor"], "foreign");
    assert_eq!(links["copilot"], "ok");

    fs::remove_file(&managed_link).unwrap();
    assert_eq!(
        named_skill(&skill::status(&fixture.home), "company-outputs")["links"]["copilot"],
        "missing"
    );
    let repaired = skill::sync(&fixture.home, false, Some("company-outputs")).unwrap();
    assert_eq!(repaired["wrote"], false);
    assert_eq!(
        fs::read_link(&managed_link).unwrap(),
        fixture.canonical("company-outputs")
    );
    assert!(fs::symlink_metadata(&foreign_file).unwrap().is_file());
    assert!(fs::symlink_metadata(&foreign_dir).unwrap().is_dir());
    for (path, bytes, mtime) in [
        (&foreign_file, b"foreign file\0\xff".as_slice(), file_mtime),
        (
            &foreign_child,
            b"foreign directory contents\0\xfe".as_slice(),
            child_mtime,
        ),
        (
            &unrelated,
            b"unrelated provider file".as_slice(),
            unrelated_mtime,
        ),
        (&extra, b"unlisted canonical file".as_slice(), extra_mtime),
    ] {
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), mtime);
    }
    assert_eq!(
        fs::read(wrong_target.join("keep.bin")).unwrap(),
        b"not a bundled skill"
    );
}

#[test]
fn scoped_cli_installs_discoverable_company_guidance_and_dependency_without_refreshing_protocol() {
    let fixture = Fixture::new();
    let protocol = fixture.canonical("cadence").join("SKILL.md");
    let custom = b"operator-managed core protocol\n";
    write_file(&protocol, custom);
    let old = age_file(&protocol);

    let installed = fixture.cli_json(&["skill", "install", "company-outputs"]);
    let selected = installed["skills"].as_array().unwrap();
    assert_eq!(selected.len(), 2);
    named_skill(&installed, "company-outputs");
    named_skill(&installed, "business-documents");
    assert_eq!(fs::read(&protocol).unwrap(), custom);
    assert_eq!(fs::metadata(&protocol).unwrap().modified().unwrap(), old);
    assert!(fixture
        .canonical("business-documents")
        .join("OPERATOR.md")
        .is_file());
    assert!(!fixture
        .canonical("business-documents")
        .join("scripts")
        .exists());

    for provider in PROVIDERS {
        for name in ["company-outputs", "business-documents"] {
            let visible = fixture.provider(provider, name);
            assert_eq!(fs::read_link(&visible).unwrap(), fixture.canonical(name));
            assert!(visible.join("SKILL.md").is_file());
        }
        for reference in ["workflow.md", "cadence.md", "proposal.md", "quotation.md"] {
            assert!(fixture
                .provider(provider, "company-outputs")
                .join("references")
                .join(reference)
                .is_file());
        }
        assert!(fixture
            .provider(provider, "business-documents")
            .join("OPERATOR.md")
            .is_file());
        assert!(!fixture.provider(provider, "cadence").exists());
    }
    let status = fixture.cli_json(&["skill", "status"]);
    assert_eq!(
        named_skill(&status, "company-outputs")["content_match"],
        true
    );
    assert_eq!(
        named_skill(&status, "business-documents")["content_match"],
        true
    );
    assert_eq!(named_skill(&status, "cadence")["content_match"], false);
}

#[test]
fn invalid_targets_are_rejected_before_writing_home() {
    let fixture = Fixture::new();
    assert!(skill::sync(&fixture.home, true, Some("../company-outputs")).is_err());
    assert_home_empty(&fixture);
    let output = fixture.cli(&["skill", "install", "not-a-bundled-skill"]);
    assert!(!output.status.success());
    assert_home_empty(&fixture);
}

#[test]
fn setup_observes_partial_and_stale_installs_without_refreshing_them() {
    let fixture = Fixture::new();
    let reference = fixture
        .canonical("company-outputs")
        .join("references/workflow.md");
    let custom = b"operator reference under construction\n";
    write_file(&reference, custom);
    let old = age_file(&reference);
    let check = setup::checks()
        .into_iter()
        .find(|check| check.name == "skill")
        .unwrap();
    let context = fixture.setup_context();
    let partial = check.run(&context, &[]);
    assert!(matches!(partial.status, setup::Status::Unknown));
    assert_eq!(fs::read(&reference).unwrap(), custom);
    assert_eq!(fs::metadata(&reference).unwrap().modified().unwrap(), old);
    assert!(!fixture
        .canonical("company-outputs")
        .join("SKILL.md")
        .exists());
    assert!(!fixture.canonical("cadence").exists());
    assert!(!fixture.canonical("business-documents").exists());
    for provider in PROVIDERS {
        assert!(!fixture.home.join(format!(".{provider}")).exists());
    }

    skill::sync(&fixture.home, false, None).unwrap();
    fs::write(&reference, custom).unwrap();
    let old = age_file(&reference);
    let stale = check.run(&context, &[]);
    assert!(matches!(stale.status, setup::Status::Unknown));
    assert_eq!(fs::read(&reference).unwrap(), custom);
    assert_eq!(fs::metadata(&reference).unwrap().modified().unwrap(), old);
    assert_eq!(skill::status(&fixture.home)["content_match"], false);
}

#[test]
fn sandbox_setup_does_not_install_missing_guidance_into_home() {
    let fixture = Fixture::new();
    let mut context = fixture.setup_context();
    context.sandbox = Some("company-guidance-test".into());
    let check = setup::checks()
        .into_iter()
        .find(|check| check.name == "skill")
        .unwrap();
    let outcome = check.run(&context, &[]);
    assert!(matches!(outcome.status, setup::Status::Unknown));
    assert_home_empty(&fixture);
}
