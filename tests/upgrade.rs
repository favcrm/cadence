//! CAD-334: `cadence upgrade` against a fake release source and temp
//! install dirs. Nothing here calls GitHub or touches `~/.local`.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cadence_agent::upgrade::{
    self, ArtifactState, Job, Layout, OnMain, ReleaseSource, Request, Run, Target,
};
use cadence_agent::{Error, Result};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";
const OLD: &str = "2222222222222222222222222222222222222222";

/// A stand-in binary: `--version` names the commit it claims to be.
fn fake_binary(sha: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"cadence 0.1.0+{sha}\"\n").into_bytes()
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Artifact files as CI's `release-artifact` job writes them.
fn write_artifact(dir: &Path, built_from: &str, claimed: &str) {
    fs::create_dir_all(dir).unwrap();
    let bin = fake_binary(built_from);
    fs::write(dir.join("cadence"), &bin).unwrap();
    fs::write(
        dir.join("cadence.sha256"),
        format!("{}  cadence\n", hex(&bin)),
    )
    .unwrap();
    fs::write(
        dir.join("manifest.json"),
        serde_json::json!({
            "source_sha": claimed,
            "run_id": 42,
            "run_attempt": 1,
            "rustc": "rustc 1.90.0",
            "cargo": "cargo 1.90.0",
            "features": ["ui"],
            "target": "x86_64-linux",
            "checks": ["fmt", "clippy", "test", "build", "ui"],
            "sha256": hex(&bin),
            "built_at": "2026-09-23T00:00:00Z",
        })
        .to_string(),
    )
    .unwrap();
}

/// Canned GitHub: one green push run on main for `SHA` holding the
/// artifact from `artifact_dir`. Each field can be turned bad.
struct Fake {
    artifact_dir: PathBuf,
    auth_ok: bool,
    on_main: OnMain,
    test_conclusion: &'static str,
    artifact: ArtifactState,
    attestation_ok: bool,
    /// `compare(resolved, linked)` answers `ahead`: the resolved sha is
    /// older than the linked one.
    backwards: bool,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn new(artifact_dir: &Path) -> Self {
        Self {
            artifact_dir: artifact_dir.to_path_buf(),
            auth_ok: true,
            on_main: OnMain::Yes,
            test_conclusion: "success",
            artifact: ArtifactState::Present,
            attestation_ok: true,
            backwards: false,
            calls: RefCell::new(Vec::new()),
        }
    }
    fn called(&self, what: &str) -> bool {
        self.calls.borrow().iter().any(|c| c == what)
    }
    fn log(&self, what: &str) {
        self.calls.borrow_mut().push(what.to_string());
    }
}

fn run_for(sha: &str) -> Run {
    Run {
        id: 42,
        attempt: 1,
        head_sha: sha.to_string(),
        head_branch: "main".into(),
        event: "push".into(),
        status: "completed".into(),
        conclusion: "success".into(),
    }
}

impl ReleaseSource for Fake {
    fn repo(&self) -> &str {
        "favcrm/cadence"
    }
    fn check_auth(&self) -> Result<()> {
        self.log("auth");
        if self.auth_ok {
            Ok(())
        } else {
            Err(Error::rejected(
                "gh is not authenticated for github.com — run `gh auth login`",
            ))
        }
    }
    fn latest_green_main(&self) -> Result<Option<Run>> {
        self.log("latest");
        Ok(Some(run_for(SHA)))
    }
    fn on_main(&self, _sha: &str) -> Result<OnMain> {
        self.log("on_main");
        Ok(self.on_main.clone())
    }
    fn compare(&self, _base: &str, _head: &str) -> Result<Option<String>> {
        self.log("compare");
        Ok(Some(if self.backwards { "ahead" } else { "behind" }.into()))
    }
    fn main_runs(&self, sha: &str) -> Result<Vec<Run>> {
        self.log("runs");
        Ok(vec![run_for(sha)])
    }
    fn jobs(&self, _run_id: u64) -> Result<Vec<Job>> {
        self.log("jobs");
        Ok(vec![
            Job {
                name: "fmt".into(),
                status: "completed".into(),
                conclusion: "success".into(),
            },
            Job {
                name: "test".into(),
                status: "completed".into(),
                conclusion: self.test_conclusion.into(),
            },
        ])
    }
    fn artifact(&self, _run_id: u64, name: &str) -> Result<ArtifactState> {
        self.log("artifact");
        // CI only ever built SHA; any other sha has no artifact.
        if name != upgrade::artifact_name(SHA) {
            return Ok(ArtifactState::Missing);
        }
        Ok(self.artifact.clone())
    }
    fn download(&self, _run_id: u64, _name: &str, dest: &Path) -> Result<()> {
        self.log("download");
        for entry in fs::read_dir(&self.artifact_dir).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), dest.join(entry.file_name())).unwrap();
        }
        Ok(())
    }
    fn verify_attestation(&self, binary: &Path, sha: &str) -> Result<String> {
        self.log("attest");
        // Only the bytes CI built for SHA carry an attestation; a
        // hand-built or planted binary does not.
        let ci_build = fs::read(self.artifact_dir.join("cadence")).ok();
        if self.attestation_ok && sha == SHA && ci_build == Some(fs::read(binary).unwrap()) {
            Ok(format!("verified {sha}"))
        } else {
            Err(Error::rejected(
                "attestation did not verify for favcrm/cadence — refusing to install",
            ))
        }
    }
}

struct Env {
    _root: TempDir,
    layout: Layout,
    artifact: PathBuf,
}

/// Temp install: `<root>/share/releases/<OLD>/cadence` is live, and the
/// link `<root>/bin/cadence` points at it — the live host's shape.
fn env() -> Env {
    let root = TempDir::new().unwrap();
    let layout = Layout {
        releases: root.path().join("share/releases"),
        link: root.path().join("bin/cadence"),
    };
    let old = layout.release_dir(OLD);
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("cadence"), fake_binary(OLD)).unwrap();
    fs::set_permissions(old.join("cadence"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(layout.link.parent().unwrap()).unwrap();
    symlink(old.join("cadence"), &layout.link).unwrap();
    let artifact = root.path().join("artifact");
    write_artifact(&artifact, SHA, SHA);
    Env {
        _root: root,
        layout,
        artifact,
    }
}

fn req(target: Target, dry_run: bool) -> Request {
    Request {
        target,
        dry_run,
        allow_unattested: false,
        backup_state_dir: None,
    }
}

fn req_unattested(sha: &str) -> Request {
    Request {
        target: Target::Sha(sha.into()),
        dry_run: false,
        allow_unattested: true,
        backup_state_dir: None,
    }
}

fn link_target(layout: &Layout) -> PathBuf {
    fs::read_link(&layout.link).unwrap()
}

/// One path: its bytes (files), mode, and link target (symlinks).
type Entry = (PathBuf, Option<Vec<u8>>, u32, Option<PathBuf>);

/// Every path under `dir`.
fn snapshot(dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        let meta = fs::symlink_metadata(&p).unwrap();
        let link = meta
            .file_type()
            .is_symlink()
            .then(|| fs::read_link(&p).unwrap());
        let bytes = meta.is_file().then(|| fs::read(&p).unwrap());
        out.push((p.clone(), bytes, meta.permissions().mode(), link));
        if meta.is_dir() {
            for e in fs::read_dir(&p).unwrap() {
                stack.push(e.unwrap().path());
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn refusal(result: Result<serde_json::Value>) -> String {
    match result {
        Ok(v) => panic!("expected a refusal, got {v}"),
        Err(e) => e.to_string(),
    }
}

fn assert_untouched(e: &Env) {
    assert_eq!(link_target(&e.layout), e.layout.binary(OLD));
    assert!(
        !e.layout.release_dir(SHA).exists(),
        "a refused upgrade must not create the release dir"
    );
}

#[test]
fn installs_verified_artifact_and_repoints_link() {
    let e = env();
    let fake = Fake::new(&e.artifact);
    let report = upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), false)).unwrap();

    let installed = e.layout.binary(SHA);
    assert_eq!(report["from_sha"], OLD);
    assert_eq!(report["to_sha"], SHA);
    assert_eq!(report["source"], "ci-artifact");
    assert_eq!(report["trust"], "attested CI build");
    assert!(report.get("warning").is_none());
    assert_eq!(report["installed"], true);
    assert_eq!(report["repointed"], true);
    assert_eq!(report["restarted"], false);
    assert_eq!(report["installed_path"], installed.to_str().unwrap());
    let v = &report["verified"];
    assert_eq!(v["on_main"], true);
    assert_eq!(v["sha256"], hex(&fake_binary(SHA)));
    assert_eq!(v["manifest_source_sha"], SHA);
    assert_eq!(v["attestation"], format!("verified {SHA}"));
    assert_eq!(v["version"], format!("cadence 0.1.0+{SHA}"));
    assert!(v["test_job"].as_str().unwrap().contains("success"));

    assert_eq!(fs::read(&installed).unwrap(), fake_binary(SHA));
    let mode = fs::metadata(&installed).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    assert!(e.layout.release_dir(SHA).join("manifest.json").is_file());
    assert!(e.layout.release_dir(SHA).join("cadence.sha256").is_file());
    assert_eq!(link_target(&e.layout), installed);
    // The previous release stays for rollback.
    assert!(e.layout.binary(OLD).is_file());
    // No temp names left in the release dir or next to the link.
    for dir in [
        e.layout.release_dir(SHA),
        e.layout.link.parent().unwrap().to_path_buf(),
    ] {
        for entry in fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !name.starts_with('.'),
                "leftover temp file {name} in {dir:?}"
            );
        }
    }
    // Order: nothing is executed or installed before the attestation.
    let calls = fake.calls.borrow().clone();
    let pos = |c: &str| calls.iter().position(|x| x == c).unwrap();
    assert!(pos("on_main") < pos("download"));
    assert!(pos("jobs") < pos("download"));
    assert!(pos("artifact") < pos("download"));
    assert!(pos("download") < pos("attest"));
}

#[test]
fn latest_main_resolves_newest_green_run() {
    let e = env();
    let fake = Fake::new(&e.artifact);
    let report = upgrade::run(&fake, &e.layout, &req(Target::LatestMain, false)).unwrap();
    assert_eq!(report["to_sha"], SHA);
    assert!(fake.called("latest"));
    assert_eq!(link_target(&e.layout), e.layout.binary(SHA));
}

#[test]
fn refuses_checksum_mismatch() {
    let e = env();
    fs::write(
        e.artifact.join("cadence.sha256"),
        format!("{}  cadence\n", "0".repeat(64)),
    )
    .unwrap();
    let fake = Fake::new(&e.artifact);
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(SHA.into()), false),
    ));
    assert!(msg.contains("sha256 mismatch"), "{msg}");
    assert!(
        !fake.called("attest"),
        "a bad checksum stops before attestation"
    );
    assert_untouched(&e);
}

#[test]
fn refuses_tampered_binary() {
    let e = env();
    // The sha256 file and manifest describe the real build; the binary
    // itself was swapped.
    fs::write(e.artifact.join("cadence"), fake_binary(OLD)).unwrap();
    let fake = Fake::new(&e.artifact);
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(SHA.into()), false),
    ));
    assert!(msg.contains("sha256 mismatch"), "{msg}");
    assert_untouched(&e);
}

#[test]
fn refuses_manifest_sha_mismatch() {
    let e = env();
    write_artifact(&e.artifact, SHA, OLD);
    let fake = Fake::new(&e.artifact);
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(SHA.into()), false),
    ));
    assert!(msg.contains("manifest source_sha"), "{msg}");
    assert!(msg.contains(OLD), "{msg}");
    assert_untouched(&e);
}

#[test]
fn refuses_when_attestation_fails() {
    let e = env();
    let mut fake = Fake::new(&e.artifact);
    fake.attestation_ok = false;
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(SHA.into()), false),
    ));
    assert!(msg.contains("attestation did not verify"), "{msg}");
    assert_untouched(&e);
}

#[test]
fn refuses_binary_that_reports_another_commit() {
    let e = env();
    // Attested and checksummed, but built from OLD while claiming SHA.
    write_artifact(&e.artifact, OLD, SHA);
    let fake = Fake::new(&e.artifact);
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(SHA.into()), false),
    ));
    assert!(msg.contains("--version reports"), "{msg}");
    assert_untouched(&e);
}

#[test]
fn refuses_sha_not_on_main() {
    let e = env();
    for (state, want) in [
        (OnMain::No("diverged".into()), "is not on main"),
        (OnMain::Unknown, "not a commit GitHub knows"),
    ] {
        let mut fake = Fake::new(&e.artifact);
        fake.on_main = state;
        let msg = refusal(upgrade::run(
            &fake,
            &e.layout,
            &req(Target::Sha(SHA.into()), false),
        ));
        assert!(msg.contains(want), "{msg}");
        assert!(!fake.called("download"));
        assert_untouched(&e);
    }
}

#[test]
fn refuses_when_test_job_did_not_pass() {
    let e = env();
    for conclusion in ["failure", "cancelled", ""] {
        let mut fake = Fake::new(&e.artifact);
        fake.test_conclusion = conclusion;
        let msg = refusal(upgrade::run(
            &fake,
            &e.layout,
            &req(Target::Sha(SHA.into()), false),
        ));
        assert!(msg.contains("`test` job has not passed"), "{msg}");
        assert!(!fake.called("download"));
        assert_untouched(&e);
    }
}

#[test]
fn refuses_expired_or_missing_artifact() {
    let e = env();
    for (state, want) in [
        (ArtifactState::Expired, "has expired"),
        (ArtifactState::Missing, "has no artifact"),
    ] {
        let mut fake = Fake::new(&e.artifact);
        fake.artifact = state;
        let msg = refusal(upgrade::run(
            &fake,
            &e.layout,
            &req(Target::Sha(SHA.into()), false),
        ));
        assert!(msg.contains(want), "{msg}");
        assert_untouched(&e);
    }
}

#[test]
fn refuses_without_gh_auth() {
    let e = env();
    let mut fake = Fake::new(&e.artifact);
    fake.auth_ok = false;
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(SHA.into()), false),
    ));
    assert!(msg.contains("gh auth login"), "{msg}");
    assert_untouched(&e);
}

#[test]
fn refuses_abbreviated_or_option_like_sha() {
    let e = env();
    let fake = Fake::new(&e.artifact);
    for bad in ["1111111", "--repo=evil", &SHA.to_uppercase(), "main"] {
        let msg = refusal(upgrade::run(
            &fake,
            &e.layout,
            &req(Target::Sha(bad.into()), false),
        ));
        assert!(msg.contains("full 40-character"), "{msg}");
    }
    assert!(fake.calls.borrow().is_empty());
    assert_untouched(&e);
}

#[test]
fn refuses_to_replace_a_non_symlink() {
    let e = env();
    fs::remove_file(&e.layout.link).unwrap();
    fs::write(&e.layout.link, b"a hand-copied binary").unwrap();
    let fake = Fake::new(&e.artifact);
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(SHA.into()), false),
    ));
    assert!(msg.contains("not a symlink"), "{msg}");
    assert_eq!(fs::read(&e.layout.link).unwrap(), b"a hand-copied binary");
}

#[test]
fn dry_run_verifies_and_changes_nothing() {
    let e = env();
    let root = e
        .layout
        .releases
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let before = snapshot(&root);
    let fake = Fake::new(&e.artifact);
    let report = upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), true)).unwrap();
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["installed"], false);
    assert_eq!(report["repointed"], false);
    assert_eq!(report["would_install"], true);
    assert_eq!(report["would_repoint"], true);
    // The full chain still ran, against a temp copy.
    assert!(fake.called("attest"));
    assert_eq!(
        report["verified"]["version"],
        format!("cadence 0.1.0+{SHA}")
    );
    assert_eq!(snapshot(&root), before, "dry run changed the install tree");
}

#[test]
fn explicit_rollback_attests_installed_releases_without_download() {
    let e = env();
    let fake = Fake::new(&e.artifact);
    upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), false)).unwrap();
    assert_eq!(link_target(&e.layout), e.layout.binary(SHA));

    // OLD was built by hand: no attestation, and CI has no artifact for
    // it. A plain rollback refuses and names the explicit opt-in.
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(OLD.into()), false),
    ));
    assert!(msg.contains("--allow-unattested"), "{msg}");
    assert!(msg.contains("not an attested CI build"), "{msg}");
    assert_eq!(link_target(&e.layout), e.layout.binary(SHA));

    // With the opt-in it rolls back, labelled, without a download.
    fake.calls.borrow_mut().clear();
    let report = upgrade::run(&fake, &e.layout, &req_unattested(OLD)).unwrap();
    assert!(!fake.called("download"));
    assert_eq!(report["source"], "installed-release");
    assert_eq!(report["trust"], "unattested local release");
    assert!(report["warning"].as_str().unwrap().contains("NOT verified"));
    assert!(report["verified"]["attestation"]
        .as_str()
        .unwrap()
        .starts_with("failed:"));
    assert!(report["verified"]["manifest"]
        .as_str()
        .unwrap()
        .starts_with("absent"));
    assert_eq!(report["from_sha"], SHA);
    assert_eq!(report["to_sha"], OLD);
    assert_eq!(link_target(&e.layout), e.layout.binary(OLD));

    // Forward again to SHA: recorded checksum and manifest, then the
    // attestation on the installed copy — still no download.
    fake.calls.borrow_mut().clear();
    let report = upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), false)).unwrap();
    assert!(fake.called("attest"));
    assert!(!fake.called("download"));
    assert_eq!(report["source"], "installed-release");
    assert_eq!(report["trust"], "attested CI build");
    assert_eq!(report["verified"]["attestation"], format!("verified {SHA}"));
    assert_eq!(report["verified"]["manifest_source_sha"], SHA);
    assert_eq!(link_target(&e.layout), e.layout.binary(SHA));

    // Re-running on the current release is a no-op for the link.
    let report = upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), false)).unwrap();
    assert_eq!(report["repointed"], false);
}

#[test]
fn offline_rollback_is_labelled_unattested() {
    let e = env();
    let mut fake = Fake::new(&e.artifact);
    fake.auth_ok = false;
    let report = upgrade::run(&fake, &e.layout, &req(Target::Sha(OLD.into()), false)).unwrap();
    assert!(!fake.called("attest"));
    assert!(!fake.called("download"));
    assert_eq!(report["trust"], "unattested local release");
    assert!(report["warning"].as_str().unwrap().contains("NOT verified"));
    assert!(report["verified"]["attestation"]
        .as_str()
        .unwrap()
        .starts_with("skipped: offline"));
    assert_eq!(link_target(&e.layout), e.layout.binary(OLD));
}

/// The reviewer's probe: a non-CI script planted at the release path,
/// claiming `+<sha>`, with no recorded files. `--latest-main` must not
/// trust (or even run) it; it downloads, attests and replaces it.
#[test]
fn latest_main_replaces_a_planted_local_release() {
    let e = env();
    let marker = e.layout.releases.join("planted-ran");
    let planted = e.layout.release_dir(SHA);
    fs::create_dir_all(&planted).unwrap();
    fs::write(
        planted.join("cadence"),
        format!(
            "#!/bin/sh\ntouch '{}'\necho \"cadence 0.1.0+{SHA}\"\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(planted.join("cadence"), fs::Permissions::from_mode(0o755)).unwrap();

    let fake = Fake::new(&e.artifact);
    let report = upgrade::run(&fake, &e.layout, &req(Target::LatestMain, false)).unwrap();
    assert!(fake.called("download"));
    assert!(fake.called("attest"));
    assert_eq!(report["source"], "ci-artifact");
    assert_eq!(report["trust"], "attested CI build");
    assert!(report["verified"]["local_release"]
        .as_str()
        .unwrap()
        .contains("no CI manifest"));
    assert_eq!(fs::read(e.layout.binary(SHA)).unwrap(), fake_binary(SHA));
    assert!(e.layout.release_dir(SHA).join("manifest.json").is_file());
    assert_eq!(link_target(&e.layout), e.layout.binary(SHA));
    assert!(!marker.exists(), "the planted binary was executed");
}

/// Recorded files copied from CI do not help a swapped binary: with a
/// CI manifest but no attestation, `--latest-main` still replaces it.
#[test]
fn latest_main_replaces_a_local_release_that_fails_attestation() {
    let e = env();
    let fake = Fake::new(&e.artifact);
    upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), false)).unwrap();
    // Rewrite binary + checksum + manifest digest consistently, keeping
    // the CI run_id: only the attestation can tell.
    let dir = e.layout.release_dir(SHA);
    let forged = format!("#!/bin/sh\n# forged\necho \"cadence 0.1.0+{SHA}\"\n").into_bytes();
    fs::write(dir.join("cadence"), &forged).unwrap();
    fs::write(
        dir.join("cadence.sha256"),
        format!("{}  cadence\n", hex(&forged)),
    )
    .unwrap();
    let mut m: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    m["sha256"] = serde_json::json!(hex(&forged));
    fs::write(dir.join("manifest.json"), m.to_string()).unwrap();

    fake.calls.borrow_mut().clear();
    let report = upgrade::run(&fake, &e.layout, &req(Target::LatestMain, false)).unwrap();
    assert!(fake.called("download"));
    assert_eq!(report["source"], "ci-artifact");
    assert!(report["verified"]["local_release"]
        .as_str()
        .unwrap()
        .contains("attestation did not verify"));
    assert_eq!(fs::read(e.layout.binary(SHA)).unwrap(), fake_binary(SHA));
}

#[test]
fn latest_main_reuses_an_attested_ci_release() {
    let e = env();
    let fake = Fake::new(&e.artifact);
    upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), false)).unwrap();
    upgrade::repoint(&e.layout.link, &e.layout.binary(OLD)).unwrap();
    fake.calls.borrow_mut().clear();
    let report = upgrade::run(&fake, &e.layout, &req(Target::LatestMain, false)).unwrap();
    assert!(fake.called("attest"));
    assert!(!fake.called("download"));
    assert_eq!(report["source"], "installed-release");
    assert_eq!(report["trust"], "attested CI build");
    assert_eq!(link_target(&e.layout), e.layout.binary(SHA));
}

#[test]
fn latest_main_refuses_to_move_backwards() {
    let e = env();
    let mut fake = Fake::new(&e.artifact);
    // The linked OLD descends from the newest green SHA.
    fake.backwards = true;
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::LatestMain, false),
    ));
    assert!(msg.contains("refusing to move backwards"), "{msg}");
    assert!(msg.contains(&format!("--sha {SHA}")), "{msg}");
    assert!(!fake.called("download"));
    assert_untouched(&e);
    // An explicit --sha is the deliberate downgrade.
    upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), false)).unwrap();
    assert_eq!(link_target(&e.layout), e.layout.binary(SHA));
}

#[test]
fn confirm_link_points_back_when_bytes_differ() {
    let root = TempDir::new().unwrap();
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    fs::write(&a, b"previous").unwrap();
    fs::write(&b, b"not what was verified").unwrap();
    let link = root.path().join("cadence");
    upgrade::repoint(&link, &b).unwrap();
    let verified = hex(b"verified bytes");
    let msg = upgrade::confirm_link(&link, &verified, Some(&a))
        .unwrap_err()
        .to_string();
    assert!(msg.contains("pointed back"), "{msg}");
    assert_eq!(fs::read_link(&link).unwrap(), a);
    // Matching bytes pass untouched.
    upgrade::confirm_link(&link, &hex(b"previous"), None).unwrap();
    // No previous link: the bad one is removed.
    upgrade::repoint(&link, &b).unwrap();
    upgrade::confirm_link(&link, &verified, None).unwrap_err();
    assert!(fs::symlink_metadata(&link).is_err());
}

#[test]
fn rollback_refuses_a_modified_installed_release() {
    let e = env();
    let fake = Fake::new(&e.artifact);
    upgrade::run(&fake, &e.layout, &req(Target::Sha(SHA.into()), false)).unwrap();
    upgrade::run(&fake, &e.layout, &req_unattested(OLD)).unwrap();
    // Someone rewrote the installed binary after it was recorded.
    let bin = e.layout.binary(SHA);
    fs::write(&bin, fake_binary(OLD)).unwrap();
    let msg = refusal(upgrade::run(
        &fake,
        &e.layout,
        &req(Target::Sha(SHA.into()), false),
    ));
    assert!(msg.contains("sha256 mismatch"), "{msg}");
    assert_eq!(link_target(&e.layout), e.layout.binary(OLD));
}

/// The swap renames a fresh link over the old one, so a reader never
/// finds the name missing. A remove-then-create swap fails this.
#[test]
fn symlink_swap_never_leaves_the_link_missing() {
    let root = TempDir::new().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    fs::write(&a, b"a").unwrap();
    fs::write(&b, b"b").unwrap();
    let link = root.path().join("bin/cadence");
    assert!(
        upgrade::repoint(&link, &a).unwrap(),
        "creates a missing link"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let missing = Arc::new(AtomicBool::new(false));
    let reader = {
        let (stop, missing, link) = (stop.clone(), missing.clone(), link.clone());
        std::thread::spawn(move || {
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                if fs::symlink_metadata(&link).is_err() {
                    missing.store(true, Ordering::Relaxed);
                }
                reads += 1;
            }
            reads
        })
    };
    for i in 0..2000 {
        let target = if i % 2 == 0 { &b } else { &a };
        assert!(upgrade::repoint(&link, target).unwrap());
    }
    stop.store(true, Ordering::Relaxed);
    let reads = reader.join().unwrap();
    assert!(reads > 0);
    assert!(
        !missing.load(Ordering::Relaxed),
        "the link was observed missing during a swap"
    );
    assert_eq!(fs::read_link(&link).unwrap(), a);
    let names: Vec<_> = fs::read_dir(link.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("cadence")]);
}

#[test]
fn layout_reads_releases_dir_off_the_live_link() {
    let root = TempDir::new().unwrap();
    let releases = root.path().join("data/cadence/releases");
    let bin = releases.join(OLD).join("cadence");
    fs::create_dir_all(bin.parent().unwrap()).unwrap();
    fs::write(&bin, b"x").unwrap();
    let link = root.path().join("bin/cadence");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&bin, &link).unwrap();
    let layout = Layout::detect(Some(link.clone()), None).unwrap();
    assert_eq!(layout.releases, releases);
    assert_eq!(
        upgrade::current(&layout).unwrap(),
        upgrade::Current::Link {
            target: bin,
            sha: Some(OLD.into())
        }
    );
}

#[test]
fn gh_output_parsers_read_real_shapes() {
    // Shapes copied from `gh run list --json ...` / `gh run view --json jobs`.
    let runs = upgrade::parse_runs(
        br#"[{"attempt":1,"conclusion":"success","databaseId":35863889206,"event":"push","headBranch":"main","headSha":"6507d4634636700433986991a5d2ad0d51587ec3","status":"completed"}]"#,
    )
    .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, 35863889206);
    assert_eq!(runs[0].head_sha, "6507d4634636700433986991a5d2ad0d51587ec3");
    let jobs = upgrade::parse_jobs(
        br#"{"jobs":[{"conclusion":"success","name":"test","status":"completed"},{"conclusion":"skipped","name":"secrets","status":"completed"}]}"#,
    )
    .unwrap();
    assert_eq!(jobs[0].name, "test");
    assert_eq!(jobs[0].conclusion, "success");
}

// ---------------------------------------------------------------------------
// CLI refusals, through the real binary with a stub `gh` on PATH.
// ---------------------------------------------------------------------------

struct Cli {
    root: TempDir,
    layout: Layout,
}

fn cli_env(gh_script: &str) -> Cli {
    let root = TempDir::new().unwrap();
    // The live-shaped install: link → releases/<OLD>/cadence.
    let layout = Layout {
        releases: root.path().join("share/releases"),
        link: root.path().join("bin/cadence"),
    };
    let old = layout.release_dir(OLD);
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("cadence"), fake_binary(OLD)).unwrap();
    fs::set_permissions(old.join("cadence"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(layout.link.parent().unwrap()).unwrap();
    symlink(old.join("cadence"), &layout.link).unwrap();
    let ghdir = root.path().join("ghbin");
    fs::create_dir_all(&ghdir).unwrap();
    fs::write(ghdir.join("gh"), gh_script).unwrap();
    fs::set_permissions(ghdir.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    Cli { root, layout }
}

fn cadence(cli: &Cli, args: &[&str]) -> std::process::Output {
    let path = format!(
        "{}:{}",
        cli.root.path().join("ghbin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(cli.root.path().join("state"))
        .arg("upgrade")
        .args(args)
        .arg("--link")
        .arg(&cli.layout.link)
        .arg("--releases-dir")
        .arg(&cli.layout.releases)
        .env("PATH", path)
        .env("HOME", cli.root.path())
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        .output()
        .unwrap()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn cli_refuses_without_gh_auth() {
    let cli = cli_env(
        "#!/bin/sh\nif [ \"$1\" = auth ]; then echo 'not logged in' >&2; exit 1; fi\nexit 0\n",
    );
    let out = cadence(&cli, &["--sha", SHA]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("gh auth login"), "{}", stderr(&out));
    assert_eq!(
        fs::read_link(&cli.layout.link).unwrap(),
        cli.layout.binary(OLD)
    );
}

#[test]
fn cli_refuses_sha_not_on_main() {
    // auth ok; compare reports the sha diverged from main.
    let cli = cli_env(
        "#!/bin/sh\ncase \"$1\" in\n  auth) exit 0 ;;\n  api) echo diverged; exit 0 ;;\n  *) echo \"unexpected gh $*\" >&2; exit 3 ;;\nesac\n",
    );
    let out = cadence(&cli, &["--sha", SHA]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("is not on main"), "{err}");
    assert!(err.contains("diverged"), "{err}");
    assert!(!cli.layout.release_dir(SHA).exists());
}

#[test]
fn cli_refuses_a_test_job_that_did_not_pass() {
    let cli = cli_env(&format!(
        "#!/bin/sh\ncase \"$1 $2\" in\n  'auth status') exit 0 ;;\n  'api repos/favcrm/cadence/compare/{SHA}...main') echo ahead ;;\n  'run list') echo '[{{\"attempt\":1,\"conclusion\":\"failure\",\"databaseId\":7,\"event\":\"push\",\"headBranch\":\"main\",\"headSha\":\"{SHA}\",\"status\":\"completed\"}}]' ;;\n  'run view') echo '{{\"jobs\":[{{\"name\":\"test\",\"status\":\"completed\",\"conclusion\":\"failure\"}}]}}' ;;\n  *) echo \"unexpected gh $*\" >&2; exit 3 ;;\nesac\n"
    ));
    let out = cadence(&cli, &["--sha", SHA]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("`test` job has not passed"), "{err}");
    assert!(err.contains("run 7: test failure"), "{err}");
}

#[test]
fn cli_restart_without_identity_refuses_before_installing() {
    // gh would fail loudly if it were reached.
    let cli = cli_env("#!/bin/sh\necho reached >&2\nexit 9\n");
    let out = cadence(&cli, &["--sha", OLD, "--restart"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("refused before installing"), "{err}");
    assert!(err.contains("--as"), "{err}");
    assert!(!err.contains("reached"), "{err}");
}

#[test]
fn cli_rollback_prints_restart_command_and_never_restarts() {
    // gh is unusable (every call fails): an offline rollback, labelled.
    let cli = cli_env("#!/bin/sh\necho reached >&2\nexit 9\n");
    // Pretend a newer release is live, then roll back to OLD.
    let newer = cli.layout.release_dir(SHA);
    fs::create_dir_all(&newer).unwrap();
    fs::write(newer.join("cadence"), fake_binary(SHA)).unwrap();
    fs::set_permissions(newer.join("cadence"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&cli.layout.link).unwrap();
    symlink(newer.join("cadence"), &cli.layout.link).unwrap();

    let out = cadence(&cli, &["--sha", OLD]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["from_sha"], SHA);
    assert_eq!(report["to_sha"], OLD);
    assert_eq!(report["trust"], "unattested local release");
    assert!(report["verified"]["attestation"]
        .as_str()
        .unwrap()
        .starts_with("skipped: offline"));
    assert_eq!(report["restarted"], false);
    assert_eq!(
        report["restart_command"],
        "cadence daemon restart --when-idle --ui"
    );
    assert_eq!(
        fs::read_link(&cli.layout.link).unwrap(),
        cli.layout.binary(OLD)
    );
    assert!(!stderr(&out).contains("reached"));
}

#[test]
fn cli_requires_exactly_one_target() {
    let cli = cli_env("#!/bin/sh\nexit 9\n");
    let none = cadence(&cli, &[]);
    assert!(!none.status.success());
    let both = cadence(&cli, &["--sha", SHA, "--latest-main"]);
    assert!(!both.status.success());
    assert!(
        stderr(&both).contains("cannot be used with"),
        "{}",
        stderr(&both)
    );
}

// ---- CAD-314: the self-update takes a backup first ----

/// A state dir holding a current-schema store, closed again.
fn state_with_store(root: &Path) -> PathBuf {
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    drop(cadence_agent::store::Store::open(&state.join("cadence.sqlite3")).unwrap());
    state
}

fn with_backup(state: &Path, dry_run: bool) -> Request {
    Request {
        backup_state_dir: Some(state.to_path_buf()),
        ..req(Target::Sha(SHA.into()), dry_run)
    }
}

#[test]
fn cad314_upgrade_takes_a_verified_backup_before_moving_the_link() {
    let e = env();
    let root = TempDir::new().unwrap();
    let state = state_with_store(root.path());
    let fake = Fake::new(&e.artifact);

    let report = upgrade::run(&fake, &e.layout, &with_backup(&state, false)).unwrap();

    assert_eq!(link_target(&e.layout), e.layout.binary(SHA));
    let manifest = PathBuf::from(report["backup"]["manifest"].as_str().unwrap());
    assert_eq!(manifest.parent(), Some(state.join("backups").as_path()));
    let m: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(m["reason"], "pre-update");
    assert_eq!(m["integrity_check"], "ok");
}

#[test]
fn cad314_upgrade_refuses_when_the_pre_update_backup_fails() {
    let e = env();
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("cadence.sqlite3"), b"not a database").unwrap();
    let fake = Fake::new(&e.artifact);

    let err = refusal(upgrade::run(&fake, &e.layout, &with_backup(&state, false)));

    assert!(err.contains("pre-update backup"), "{err}");
    assert_untouched(&e);
}

#[test]
fn cad314_upgrade_dry_run_takes_no_backup() {
    let e = env();
    let root = TempDir::new().unwrap();
    let state = state_with_store(root.path());
    let fake = Fake::new(&e.artifact);

    let report = upgrade::run(&fake, &e.layout, &with_backup(&state, true)).unwrap();

    assert!(report["backup"].is_null(), "{report}");
    assert!(!state.join("backups").exists());
}
