//! CAD-561: `cadence update` — the pipeline against a fake release
//! source, a fake daemon and temp install dirs. Nothing here calls
//! GitHub, touches `~/.local`, starts a daemon or restarts anything.
//!
//! Run with `--features test-seam`: the lease claim is an operator
//! action, and the suite asserts the operator in-band (CAD-482) exactly
//! as it does in CI.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cadence_agent::error::{Error, Result};
use cadence_agent::store::Store;
use cadence_agent::test_seam::{self, Asserted};
use cadence_agent::update::{self, CheckReport, Options, PendingUpdate, UpdateHost, Waiter};
use cadence_agent::upgrade::{ArtifactState, Job, Layout, OnMain, ReleaseSource, Run};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn fake_binary(sha: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"cadence 0.1.0+{sha}\"\n").into_bytes()
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The artifact files as CI's `release-artifact` job writes them.
fn write_artifact(dir: &Path, sha: &str) {
    fs::create_dir_all(dir).unwrap();
    let bin = fake_binary(sha);
    let binary = dir.join("cadence");
    fs::write(&binary, &bin).unwrap();
    // `install_files` writes the binary 0755; a release on disk is too.
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        dir.join("cadence.sha256"),
        format!("{}  cadence\n", hex(&bin)),
    )
    .unwrap();
    fs::write(
        dir.join("manifest.json"),
        serde_json::json!({
            "source_sha": sha,
            "run_id": 42,
            "run_attempt": 1,
            "target": "x86_64-linux",
            "sha256": hex(&bin),
        })
        .to_string(),
    )
    .unwrap();
}

/// Point the link at a release, replacing whatever it pointed at.
fn link_to(layout: &Layout, sha: &str) {
    let _ = fs::remove_file(&layout.link);
    symlink(layout.binary(sha), &layout.link).unwrap();
}

/// A release already installed under `<releases>/<sha>` (a previous
/// update's), with its CI records.
fn install_release(layout: &Layout, sha: &str) {
    let dir = layout.release_dir(sha);
    write_artifact(&dir, sha);
}

/// Canned GitHub: one green push run on main holding the artifact.
struct Fake {
    auth_ok: bool,
    on_main: OnMain,
    test_conclusion: &'static str,
    artifact: ArtifactState,
    attestation_ok: bool,
    titles: Vec<String>,
    schema: Option<i64>,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            auth_ok: true,
            on_main: OnMain::Yes,
            test_conclusion: "success",
            artifact: ArtifactState::Present,
            attestation_ok: true,
            titles: vec!["CAD-560: a merged change (#320)".into()],
            schema: Some(18),
            calls: RefCell::new(Vec::new()),
        }
    }
    fn log(&self, what: &str) {
        self.calls.borrow_mut().push(what.to_string());
    }
    fn called(&self, what: &str) -> bool {
        self.calls.borrow().iter().any(|c| c == what)
    }
}

impl ReleaseSource for Fake {
    fn repo(&self) -> &str {
        "favcrm/cadence"
    }
    fn check_auth(&self) -> Result<()> {
        self.log("check_auth");
        if self.auth_ok {
            Ok(())
        } else {
            Err(Error::rejected("gh is not authenticated"))
        }
    }
    fn latest_green_main(&self) -> Result<Option<Run>> {
        self.log("latest_green_main");
        Ok(Some(Run {
            id: 42,
            attempt: 1,
            head_sha: NEW.into(),
            head_branch: "main".into(),
            event: "push".into(),
            status: "completed".into(),
            conclusion: "success".into(),
        }))
    }
    fn on_main(&self, _sha: &str) -> Result<OnMain> {
        Ok(self.on_main.clone())
    }
    fn compare(&self, _base: &str, _head: &str) -> Result<Option<String>> {
        Ok(Some("ahead".into()))
    }
    fn main_runs(&self, sha: &str) -> Result<Vec<Run>> {
        Ok(vec![Run {
            id: 42,
            attempt: 1,
            head_sha: sha.into(),
            head_branch: "main".into(),
            event: "push".into(),
            status: "completed".into(),
            conclusion: "success".into(),
        }])
    }
    fn merge_group_runs(&self, _sha: &str) -> Result<Vec<Run>> {
        Ok(Vec::new())
    }
    fn jobs(&self, _run_id: u64) -> Result<Vec<Job>> {
        Ok(vec![Job {
            name: "test".into(),
            status: "completed".into(),
            conclusion: self.test_conclusion.into(),
        }])
    }
    fn artifact(&self, _run_id: u64, _name: &str) -> Result<ArtifactState> {
        Ok(self.artifact.clone())
    }
    fn download(&self, _run_id: u64, _name: &str, dest: &Path) -> Result<()> {
        self.log("download");
        write_artifact(dest, NEW);
        Ok(())
    }
    fn verify_attestation(&self, _binary: &Path, _sha: &str) -> Result<String> {
        self.log("verify_attestation");
        if self.attestation_ok {
            Ok("build provenance verified".into())
        } else {
            Err(Error::rejected("attestation did not verify"))
        }
    }
    fn merged_titles(&self, _base: &str, _head: &str) -> Result<Vec<String>> {
        self.log("merged_titles");
        Ok(self.titles.clone())
    }
    fn schema_version(&self, _sha: &str) -> Result<Option<i64>> {
        self.log("schema_version");
        Ok(self.schema)
    }
}

/// The whole pipeline's world: a temp state dir with a real store, temp
/// releases, and a fake daemon whose answers the test drives.
struct Host {
    _root: TempDir,
    state_dir: PathBuf,
    layout: Layout,
    source: Fake,
    identity: String,
    lines: RefCell<Vec<String>>,
    /// What the fake daemon says is in flight.
    waiters: RefCell<Vec<Waiter>>,
    /// Clear the waiter list after this many `waiters()` calls (0: never)
    /// — a turn that finishes on its own during the drain.
    waiters_clear_after: Cell<u32>,
    /// Whether the fake daemon is draining.
    draining: Cell<bool>,
    /// The marker as the fake daemon sees it.
    pending: RefCell<Option<PendingUpdate>>,
    daemon_build: RefCell<Option<String>>,
    board_build: RefCell<Option<String>>,
    /// Health answers the target after a restart (false: never).
    health_ok: Cell<bool>,
    /// Whether a restart's health check failed (set by `restart`).
    rolled_back: Cell<bool>,
    /// The board exists (None answers from `board_build`).
    board_running: Cell<bool>,
    restarts: RefCell<Vec<PathBuf>>,
    now: Cell<f64>,
    sleeps: Cell<u64>,
}

impl Host {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let state_dir = root.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let releases = root.path().join("releases");
        fs::create_dir_all(&releases).unwrap();
        let link = root.path().join("bin").join("cadence");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        let artifact_dir = root.path().join("artifact");
        write_artifact(&artifact_dir, NEW);
        // A real store, so the backup has a cadence database to copy.
        Store::open(&state_dir.join("cadence.sqlite3")).unwrap();
        // CAD-482: the suite asserts the operator in-band.
        let seam = state_dir.join("seam");
        fs::create_dir_all(&seam).unwrap();
        fs::write(seam.join("token"), "test-token").unwrap();
        let host = Self {
            _root: root,
            state_dir,
            layout: Layout { releases, link },
            source: Fake::new(),
            identity: "operator:ada".into(),
            lines: RefCell::new(Vec::new()),
            waiters: RefCell::new(Vec::new()),
            waiters_clear_after: Cell::new(0),
            draining: Cell::new(false),
            pending: RefCell::new(None),
            daemon_build: RefCell::new(Some(OLD.to_string())),
            board_build: RefCell::new(Some(OLD.to_string())),
            health_ok: Cell::new(true),
            rolled_back: Cell::new(false),
            board_running: Cell::new(true),
            restarts: RefCell::new(Vec::new()),
            now: Cell::new(cadence_agent::rollout::unix_now()),
            sleeps: Cell::new(0),
        };
        // A previous release, linked: the state every update starts in.
        install_release(&host.layout, OLD);
        link_to(&host.layout, OLD);
        host
    }

    /// Whether the last run rolled back (the tests' shorthand).
    fn rolled_back_now(&self) -> bool {
        self.rolled_back.get()
    }

    /// The lines printed so far, as one string.
    fn log(&self) -> String {
        self.lines.borrow().join("\n")
    }

    /// The lease row (holder + receipt), straight from the store.
    fn lease_row(&self) -> serde_json::Value {
        cadence_agent::rollout::status(&self.state_dir).unwrap()
    }

    fn options(&self) -> Options {
        Options::default()
    }
}

impl UpdateHost for Host {
    fn state_dir(&self) -> &Path {
        &self.state_dir
    }
    fn layout(&self) -> &Layout {
        &self.layout
    }
    fn source(&self) -> &dyn ReleaseSource {
        &self.source
    }
    fn identity(&self) -> &str {
        &self.identity
    }
    fn progress(&self, line: &str) {
        self.lines.borrow_mut().push(line.to_string());
    }
    fn waiters(&self) -> Result<Vec<Waiter>> {
        let list = self.waiters.borrow().clone();
        let left = self.waiters_clear_after.get();
        if left > 0 {
            self.waiters_clear_after.set(left - 1);
            if left == 1 {
                self.waiters.borrow_mut().clear();
            }
        }
        Ok(list)
    }
    fn set_pending(&self, pending: Option<&PendingUpdate>) -> Result<()> {
        *self.pending.borrow_mut() = pending.cloned();
        match pending {
            Some(p) => update::write_pending(&self.state_dir, p),
            None => {
                update::clear_pending(&self.state_dir);
                Ok(())
            }
        }
    }
    fn set_drain(&self, on: bool) -> Result<()> {
        self.draining.set(on);
        Ok(())
    }
    fn restart(&self, binary: &Path) -> Result<()> {
        self.restarts.borrow_mut().push(binary.to_path_buf());
        let sha = binary
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if self.health_ok.get() {
            *self.daemon_build.borrow_mut() = Some(sha.clone());
            if self.board_running.get() {
                *self.board_build.borrow_mut() = Some(sha);
            }
        } else {
            self.rolled_back.set(true);
        }
        Ok(())
    }
    fn daemon_build(&self) -> Result<Option<String>> {
        Ok(self.daemon_build.borrow().clone())
    }
    fn board_build(&self) -> Result<Option<String>> {
        if !self.board_running.get() {
            return Ok(None);
        }
        Ok(self.board_build.borrow().clone())
    }
    fn now(&self) -> f64 {
        self.now.get()
    }
    fn sleep(&self, duration: Duration) {
        self.sleeps.set(self.sleeps.get() + 1);
        self.now.set(self.now.get() + duration.as_secs_f64());
    }
}

/// A lease some other operator holds, as the fixture's starting state.
fn claim_other(host: &Host) {
    test_seam::scoped(Asserted::Operator, || {
        cadence_agent::rollout::claim(
            &host.state_dir,
            &cadence_agent::rollout::ClaimRequest {
                caller: &cadence_agent::rollout::Caller {
                    identity: "operator:someone-else".into(),
                    source: "as",
                },
                reason: "another rollout",
                target: Some(NEW),
                ttl: Duration::from_secs(3600),
                takeover: false,
                now: cadence_agent::rollout::unix_now(),
            },
        )
        .unwrap();
    });
}

/// Everything the pipeline does runs under the operator assertion.
fn run(host: &Host, opts: &Options) -> Result<update::RunReport> {
    test_seam::scoped(Asserted::Operator, || update::run(host, opts))
}

fn check(host: &Host) -> Result<CheckReport> {
    test_seam::scoped(Asserted::Operator, || update::check(host))
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

#[test]
fn check_reports_versions_changes_schema_and_blockers_without_changing_anything() {
    let host = Host::new();
    host.waiters.borrow_mut().push(Waiter {
        alias: "swe-554".into(),
        message: "m1".into(),
        state: "running".into(),
        age_secs: 12 * 60,
    });
    let report = check(&host).unwrap();
    assert_eq!(report.current.as_deref(), Some(OLD));
    assert_eq!(report.target, NEW);
    assert!(!report.up_to_date);
    assert_eq!(report.changes, vec!["CAD-560: a merged change (#320)"]);
    assert_eq!(report.schema_current, Some(18));
    assert_eq!(report.schema_target, Some(18));
    assert!(!report.migration);
    assert!(
        report
            .blockers
            .iter()
            .any(|b| b.contains("waiting for 1 turn")),
        "{:?}",
        report.blockers
    );
    // Nothing changed: no new release, no lease, no backup, no drain.
    assert!(!host.layout.release_dir(NEW).exists());
    assert_eq!(host.lease_row()["held"], serde_json::json!(false));
    assert!(!host.draining.get());
    assert!(!host.source.called("download"));
    assert!(!host.source.called("verify_attestation"));
    let lines = report.lines().join("\n");
    assert!(lines.contains("current: aaaa"), "{lines}");
    assert!(lines.contains("available: bbbb"), "{lines}");
    assert!(lines.contains("CAD-560"), "{lines}");
    assert!(lines.contains("schema: no change (18)"), "{lines}");
    assert!(
        lines.contains("waiting for 1 turn: swe-554 (12m)"),
        "{lines}"
    );
}

#[test]
fn check_reports_a_migration_and_a_lease_held_by_another_identity() {
    let mut host = Host::new();
    host.source.schema = Some(19);
    claim_other(&host);
    let report = test_seam::scoped(Asserted::Operator, || update::check(&host)).unwrap();
    assert!(report.migration, "schema 19 > 18");
    assert!(
        report.blockers.iter().any(
            |b| b == "update in progress by operator:someone-else since "
                || b.contains("update in progress by operator:someone-else")
        ),
        "{:?}",
        report.blockers
    );
    assert!(report.lines().join("\n").contains("migration 18 → 19"));
}

#[test]
fn an_up_to_date_check_says_so() {
    let host = Host::new();
    install_release(&host.layout, NEW);
    link_to(&host.layout, NEW);
    let report = check(&host).unwrap();
    assert!(report.up_to_date);
    assert!(report.changes.is_empty());
    assert!(report.lines().join("\n").contains("already installed"));
}

// ---------------------------------------------------------------------------
// the run: each step
// ---------------------------------------------------------------------------

#[test]
fn a_run_takes_the_lease_backs_up_installs_drains_switches_checks_health_and_releases() {
    let host = Host::new();
    host.waiters.borrow_mut().push(Waiter {
        alias: "swe-554".into(),
        message: "m1".into(),
        state: "running".into(),
        age_secs: 720,
    });
    // The turn finishes on the first drain poll (the check saw it too).
    host.waiters_clear_after.set(2);
    let report = run(&host, &Options::default()).unwrap();
    // The lease was taken for the target and released at the end.
    assert_eq!(report.check.target, NEW);
    let lease = host.lease_row();
    assert_eq!(lease["held"], serde_json::json!(false), "{lease}");
    // The backup is outside the state dir and recorded as the receipt.
    let backup_dir = update::default_backup_dir(&host.state_dir);
    assert!(backup_dir.starts_with(host._root.path()));
    assert!(!backup_dir.starts_with(&host.state_dir));
    let backups = fs::read_dir(&backup_dir).unwrap().count();
    assert_eq!(backups, 2, "one copy + one manifest");
    assert!(report.backup.is_some());
    let receipt = report.backup.as_ref().unwrap()["receipt"].clone();
    assert_eq!(receipt["recorded"], serde_json::json!(true), "{receipt}");
    assert!(receipt["schema_version"].as_i64().is_some());
    // Side by side install, link switched, previous release kept.
    assert!(host.layout.release_dir(NEW).join("cadence").is_file());
    assert_eq!(
        fs::read_link(&host.layout.link).unwrap(),
        host.layout.binary(NEW)
    );
    assert!(host.layout.release_dir(OLD).exists());
    // The drain ran, reported the waiter, then went quiet.
    assert_eq!(report.drain["timed_out"], serde_json::json!(false));
    let log = host.log();
    assert!(log.contains("waiting for 1 turn: swe-554 (12m)"), "{log}");
    assert!(log.contains("drained: quiet after"), "{log}");
    assert!(
        log.contains("switching: restarting the daemon on bbbb"),
        "{log}"
    );
    assert!(
        log.contains("health: daemon and board answer on bbbb"),
        "{log}"
    );
    assert!(log.contains("done: "), "{log}");
    // The restart ran the new release's binary.
    assert_eq!(host.restarts.borrow().as_slice(), [host.layout.binary(NEW)]);
    // The gate and the marker are lifted.
    assert!(!host.draining.get());
    assert!(update::pending_update(&host.state_dir).is_none());
    assert!(!report.rolled_back);
}

#[test]
fn a_second_run_says_already_up_to_date_and_touches_nothing() {
    let host = Host::new();
    install_release(&host.layout, NEW);
    link_to(&host.layout, NEW);
    // A finished update: the daemon answers the new build too.
    host.daemon_build.replace(Some(NEW.to_string()));
    host.board_build.replace(Some(NEW.to_string()));
    let report = run(&host, &host.options()).unwrap();
    assert!(report.check.up_to_date);
    assert!(host.log().contains("already up to date"), "{}", host.log());
    assert_eq!(host.lease_row()["held"], serde_json::json!(false));
    assert!(!host.draining.get());
    assert!(!host.source.called("download"));
    assert!(host.restarts.borrow().is_empty());
}

#[test]
fn a_rerun_finishes_an_installed_release_whose_restart_did_not_happen() {
    // The link already points at the target (a previous run installed
    // it) but the daemon still answers with the old build: the re-run
    // must finish the update — restart and health check — not stop at
    // "already up to date".
    let host = Host::new();
    install_release(&host.layout, NEW);
    link_to(&host.layout, NEW);
    assert_eq!(host.daemon_build().unwrap().as_deref(), Some(OLD));
    let report = run(&host, &host.options()).unwrap();
    assert!(
        host.log().contains("finishing the update"),
        "{}",
        host.log()
    );
    assert!(host
        .log()
        .contains("health: daemon and board answer on bbbb"));
    assert_eq!(host.restarts.borrow().as_slice(), [host.layout.binary(NEW)]);
    assert!(report.install.is_none() || report.install.as_ref().unwrap().is_null());
    assert!(!host.rolled_back_now());
    // No backup was taken for a run that installed nothing.
    assert!(!update::default_backup_dir(&host.state_dir).exists());
    assert_eq!(host.lease_row()["held"], serde_json::json!(false));
}

#[test]
fn an_update_another_identity_is_running_is_reported_not_a_raw_lease_error() {
    let host = Host::new();
    claim_other(&host);
    let err = run(&host, &host.options()).unwrap_err().to_string();
    assert!(
        err.contains("update in progress by operator:someone-else since"),
        "{err}"
    );
    assert!(
        !err.contains("rollout lease is held by"),
        "raw lease error leaked: {err}"
    );
    // Nothing was installed, backed up or drained.
    assert!(!host.layout.release_dir(NEW).join("cadence").exists());
    assert!(!update::default_backup_dir(&host.state_dir).exists());
    assert!(!host.draining.get());
    assert!(host.restarts.borrow().is_empty());
}

#[test]
fn the_drain_is_bounded_and_switches_anyway_with_what_it_waited_on() {
    let host = Host::new();
    host.waiters.borrow_mut().push(Waiter {
        alias: "swe-554".into(),
        message: "m1".into(),
        state: "running".into(),
        age_secs: 12 * 60,
    });
    let opts = Options {
        drain: Duration::from_secs(30),
        ..Options::default()
    };
    let report = run(&host, &opts).unwrap();
    assert_eq!(report.drain["timed_out"], serde_json::json!(true));
    assert_eq!(report.drain["waited_secs"], serde_json::json!(30));
    assert_eq!(
        report.drain["waiters"][0]["alias"],
        serde_json::json!("swe-554")
    );
    let log = host.log();
    assert!(log.contains("drain timeout after 30s"), "{log}");
    assert!(log.contains("swe-554 (12m) still running"), "{log}");
    assert!(log.contains("they resume after the restart"), "{log}");
    // It switched anyway: the release is installed and the link moved.
    assert_eq!(
        fs::read_link(&host.layout.link).unwrap(),
        host.layout.binary(NEW)
    );
}

#[test]
fn now_switches_immediately_without_waiting() {
    let host = Host::new();
    host.waiters.borrow_mut().push(Waiter {
        alias: "swe-554".into(),
        message: "m1".into(),
        state: "running".into(),
        age_secs: 60,
    });
    let opts = Options {
        now: true,
        drain: Duration::from_secs(600),
        ..Options::default()
    };
    let report = run(&host, &opts).unwrap();
    assert_eq!(host.sleeps.get(), 0, "no drain wait at all");
    assert_eq!(report.drain["now"], serde_json::json!(true));
    assert_eq!(report.drain["waited_secs"], serde_json::json!(0));
    assert_eq!(
        report.drain["waiters"][0]["alias"],
        serde_json::json!("swe-554")
    );
    assert!(
        host.log().contains("--now: switching with"),
        "{}",
        host.log()
    );
    assert_eq!(
        fs::read_link(&host.layout.link).unwrap(),
        host.layout.binary(NEW)
    );
}

// ---------------------------------------------------------------------------
// health check and auto-rollback
// ---------------------------------------------------------------------------

#[test]
fn a_failing_health_check_rolls_back_to_the_previous_release_and_restarts_it() {
    let host = Host::new();
    host.health_ok.set(false);
    let report = run(&host, &host.options()).unwrap();
    assert!(report.rolled_back);
    assert_eq!(report.health["ok"], serde_json::json!(false));
    assert_eq!(report.health["rolled_back_to"], serde_json::json!(OLD));
    // The link points back at the previous release, which was restarted.
    assert_eq!(
        fs::read_link(&host.layout.link).unwrap(),
        host.layout.binary(OLD)
    );
    assert_eq!(
        host.restarts.borrow().as_slice(),
        [host.layout.binary(NEW), host.layout.binary(OLD)]
    );
    let log = host.log();
    assert!(log.contains("health: health check failed"), "{log}");
    assert!(
        log.contains(&format!("rollback: repointing to {OLD}")),
        "{log}"
    );
    assert!(
        log.contains(&format!("rolled back: {OLD} is answering health again")),
        "{log}"
    );
    // The lease is still auto-released after a rollback.
    assert_eq!(host.lease_row()["held"], serde_json::json!(false));
    assert!(!host.draining.get());
}

#[test]
fn a_health_check_that_never_comes_up_reports_the_failed_rollback() {
    let host = Host::new();
    host.health_ok.set(false);
    host.daemon_build.replace(None);
    host.board_running.set(false);
    let err = run(&host, &host.options()).unwrap_err().to_string();
    assert!(err.contains("health check failed"), "{err}");
    assert_eq!(
        fs::read_link(&host.layout.link).unwrap(),
        host.layout.binary(OLD)
    );
}

// ---------------------------------------------------------------------------
// attestation and the operator gate (unchanged)
// ---------------------------------------------------------------------------

#[test]
fn an_unattested_download_is_refused_and_nothing_is_installed() {
    let mut host = Host::new();
    host.source.attestation_ok = false;
    let err = run(&host, &host.options()).unwrap_err().to_string();
    assert!(err.contains("attestation did not verify"), "{err}");
    assert!(!host.layout.release_dir(NEW).join("cadence").exists());
    assert_eq!(
        fs::read_link(&host.layout.link).unwrap(),
        host.layout.binary(OLD)
    );
    assert!(host.restarts.borrow().is_empty());
    assert_eq!(host.lease_row()["held"], serde_json::json!(false));
}

#[test]
fn an_update_never_installs_from_a_sha_that_is_not_on_main() {
    let mut host = Host::new();
    host.source.on_main = OnMain::No("diverged".into());
    let err = run(&host, &host.options()).unwrap_err().to_string();
    assert!(err.contains("is not on main"), "{err}");
    assert!(!host.layout.release_dir(NEW).join("cadence").exists());
}

// ---------------------------------------------------------------------------
// rollback and retention
// ---------------------------------------------------------------------------

#[test]
fn rollback_returns_to_the_previous_release_and_offers_the_restore_when_the_schema_moved() {
    let mut host = Host::new();
    // Two releases on disk, the newest linked.
    install_release(&host.layout, NEW);
    link_to(&host.layout, NEW);
    // The store is newer than the previous release's schema.
    host.source.schema = Some(17);
    let report = test_seam::scoped(Asserted::Operator, || update::rollback(&host)).unwrap();
    assert!(report.rolled_back);
    assert_eq!(
        fs::read_link(&host.layout.link).unwrap(),
        host.layout.binary(OLD)
    );
    assert_eq!(host.restarts.borrow().as_slice(), [host.layout.binary(OLD)]);
    let log = host.log();
    assert!(
        log.contains(&format!("rolling back: {NEW} → {OLD}")),
        "{log}"
    );
    assert!(
        log.contains(&format!("the store schema (18) is newer than {OLD}'s (17)")),
        "{log}"
    );
    assert!(log.contains("cadence restore"), "{log}");
    assert_eq!(report.health["ok"], serde_json::json!(true));
}

#[test]
fn pruning_keeps_the_linked_release_and_the_newest_previous_ones() {
    let host = Host::new();
    let mut shas = vec![OLD.to_string()];
    for i in 0..5u8 {
        let sha = format!("{:0>40}", i + 1).replace('0', "c");
        install_release(&host.layout, &sha);
        shas.push(sha);
    }
    let linked = shas.last().unwrap().clone();
    link_to(&host.layout, &linked);
    let removed =
        test_seam::scoped(Asserted::Operator, || update::prune_releases(&host, 2)).unwrap();
    // The linked release is never a candidate; `keep` previous ones stay.
    assert_eq!(removed.len(), shas.len() - 3, "{removed:?}");
    assert!(host.layout.release_dir(&linked).exists());
    for sha in &removed {
        assert!(!host.layout.release_dir(sha).exists(), "{sha} not pruned");
        assert_ne!(sha, &linked);
    }
}
