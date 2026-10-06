#![cfg(all(unix, feature = "test-seam"))]

use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cadence_agent::issue::{model::Ref, parse, start, write, Pm};
use cadence_agent::worktree::lifecycle;
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const PROJECT: &str = "retention";
const PREFIX: &str = "C84";
const ACTOR: &str = "retention-test";

#[test]
fn retention_guards_legacy_adoption_and_released_generation_restart() {
    let fx = Fixture::new();
    let (retained_lane, retained_branch) = fx.start_lane("C84-1", "retained");
    fx.commit_and_merge(&retained_lane, &retained_branch, "retained evidence");
    let retained_file = retained_lane.join("retained-evidence.txt");
    let retained_tip = fx.branch_tip(&retained_branch);
    let reason = "preserve lane for CAD-848 investigation";

    let retain = fx.retain(&retained_lane, reason);
    assert!(
        retain.status.success(),
        "retain failed: {}",
        output_text(&retain)
    );
    assert_retained(&fx, &retained_lane, reason);

    // Ordinary issue start must not reuse a retained development path.
    let mut start_command = fx.cli();
    start_command.args([
        "issue",
        "start",
        "C84-1",
        "--repo",
        fx.repo.to_str().unwrap(),
        "--owner",
        ACTOR,
        "--by",
        ACTOR,
    ]);
    let start =
        cadence_agent::reaper::output(&mut start_command).expect("run ordinary issue start CLI");
    assert_refused(&start, "recorded as retained");

    // The lifecycle begin/activate entry points and setup-failed transition
    // cannot overwrite a retained record.
    let record = lifecycle::managed_record(&fx.repo, &retained_lane)
        .expect("read retained record")
        .expect("retained record exists");
    let begin_error = lifecycle::begin(&fx.repo, record.clone())
        .expect_err("begin must refuse a retained checkout")
        .to_string();
    assert!(
        begin_error.contains("retained"),
        "unexpected begin error: {begin_error}"
    );
    let activate_error = lifecycle::activate(&fx.repo, record.clone())
        .expect_err("activate must refuse a retained checkout")
        .to_string();
    assert!(
        activate_error.contains("retained"),
        "unexpected activate error: {activate_error}"
    );
    let setup_error = lifecycle::transition(
        &fx.repo,
        &retained_lane,
        "setup-failed",
        Some("simulated setup failure"),
    )
    .expect_err("setup-failed must not overwrite a retained checkout")
    .to_string();
    assert!(
        setup_error.contains("retained"),
        "unexpected setup-failed error: {setup_error}"
    );
    assert_retained(&fx, &retained_lane, reason);

    // Retention is already in force when finish attempts release. The Git
    // observer is open so a regression cannot hang; it records any removal.
    let observer = GitObserver::new(&fx, "retained-finish", true);
    let mut finish = fx.finish_command("C84-1", &retained_lane);
    observer.configure(&mut finish);
    let finish = cadence_agent::reaper::output(&mut finish).expect("run retained finish CLI");
    assert_refused(&finish, "state retained");
    assert!(
        !observer.events.exists(),
        "finish attempted Git worktree removal for a retained lane"
    );
    assert_retained(&fx, &retained_lane, reason);
    assert_eq!(
        fs::read_to_string(&retained_file).unwrap(),
        "retained evidence\n"
    );
    assert_eq!(fx.branch_tip(&retained_branch), retained_tip);

    // Exercise the production finish path with a deterministic Git barrier.
    // The test-seam proc root supplies a complete empty process inventory; all
    // checkout, lifecycle, lock, Git, and tracker operations remain real.
    let (active_lane, active_branch) = fx.start_lane("C84-2", "release-race");
    fx.commit_and_merge(&active_lane, &active_branch, "release race evidence");
    let observer = GitObserver::new(&fx, "release-race", false);
    let mut finish_command = fx.finish_command("C84-2", &active_lane);
    observer.configure(&mut finish_command);
    finish_command.env("CADENCE_TEST_PROC_ROOT", &fx.empty_proc_root);
    let finish_child =
        cadence_agent::reaper::spawn(&mut finish_command).expect("spawn real finish CLI");
    let mut race = RaceChildren {
        finish: Some(finish_child),
        retain: None,
        adopt: None,
        gate: observer.gate.clone(),
    };

    wait_for_file(&observer.entered, race.finish.as_mut().unwrap())
        .expect("finish reaches Git worktree removal barrier");
    assert!(active_lane.is_dir(), "Git removal ran before the barrier");
    assert!(lifecycle_lock_is_held(&fx.repo).expect("probe lifecycle flock"));
    assert!(
        race.finish.as_mut().unwrap().try_wait().unwrap().is_none(),
        "finish exited while Git removal was gated"
    );

    // Start the real retention CLI while finish is parked at worktree removal.
    let mut retain_command = fx.cli();
    retain_command
        .args(["issue", "checkout", "retain", "--repo"])
        .arg(&fx.repo)
        .arg("--path")
        .arg(&active_lane)
        .args(["--reason", "retain raced with release"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    race.retain = Some(
        cadence_agent::reaper::spawn(&mut retain_command).expect("spawn concurrent retain CLI"),
    );
    assert!(
        race.retain.as_mut().unwrap().try_wait().unwrap().is_none(),
        "retain returned before the gated release could finish"
    );

    fs::write(&observer.gate, "continue\n").expect("release Git removal barrier");
    let finish = race
        .finish
        .take()
        .unwrap()
        .wait_with_output()
        .expect("wait for finish CLI");
    let retain = race
        .retain
        .take()
        .unwrap()
        .wait_with_output()
        .expect("wait for concurrent retain CLI");
    assert!(
        finish.status.success(),
        "finish failed: {}",
        output_text(&finish)
    );
    assert_refused(&retain, "released");
    assert!(!active_lane.exists(), "finish did not remove the worktree");
    let released = lifecycle::managed_record(&fx.repo, &active_lane)
        .expect("read released record")
        .expect("released record remains in lifecycle history");
    assert_eq!(released.state, "released");
    assert!(released.retention_reason.is_none());
    assert!(released
        .release_reason
        .as_deref()
        .is_some_and(|r| r.contains("checkout released")));
    assert_retained(&fx, &retained_lane, reason);

    // Legacy issue refs can authorize finish without a lifecycle record, but
    // adoption must serialize behind that deletion and revalidate afterwards.
    let (legacy_lane, legacy_branch) = fx.create_legacy_lane();
    let legacy_tip = fx.branch_tip(&legacy_branch);
    assert!(lifecycle::managed_record(&fx.repo, &legacy_lane)
        .expect("check legacy ownership")
        .is_none());
    let observer = GitObserver::new(&fx, "legacy-release-race", false);
    let mut finish_command = fx.finish_command("C84-3", &legacy_lane);
    observer.configure(&mut finish_command);
    finish_command.env("CADENCE_TEST_PROC_ROOT", &fx.empty_proc_root);
    let finish_child =
        cadence_agent::reaper::spawn(&mut finish_command).expect("spawn legacy finish CLI");
    let mut race = RaceChildren {
        finish: Some(finish_child),
        retain: None,
        adopt: None,
        gate: observer.gate.clone(),
    };
    wait_for_file(&observer.entered, race.finish.as_mut().unwrap())
        .expect("legacy finish reaches Git worktree removal barrier");
    assert!(
        legacy_lane.is_dir(),
        "legacy Git removal ran before the barrier"
    );
    assert!(lifecycle_lock_is_held(&fx.repo).expect("probe legacy lifecycle flock"));

    let mut adopt_command = fx.cli();
    adopt_command
        .args(["issue", "checkout", "adopt", "--repo"])
        .arg(&fx.repo)
        .arg("--path")
        .arg(&legacy_lane)
        .args([
            "--purpose",
            "development",
            "--tool",
            "cadence",
            "--owner",
            ACTOR,
        ])
        .arg("--pinned-sha")
        .arg(&legacy_tip)
        .arg("--branch")
        .arg(&legacy_branch)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    race.adopt = Some(
        cadence_agent::reaper::spawn(&mut adopt_command)
            .expect("spawn concurrent checkout adoption CLI"),
    );
    assert!(
        race.adopt.as_mut().unwrap().try_wait().unwrap().is_none(),
        "adoption returned while legacy deletion was gated"
    );

    fs::write(&observer.gate, "continue\n").expect("release legacy Git removal barrier");
    let finish = race
        .finish
        .take()
        .unwrap()
        .wait_with_output()
        .expect("wait for legacy finish CLI");
    let adoption = race
        .adopt
        .take()
        .unwrap()
        .wait_with_output()
        .expect("wait for concurrent adoption CLI");
    assert!(
        finish.status.success(),
        "legacy finish failed: {}",
        output_text(&finish)
    );
    assert_refused(&adoption, "checkout");
    assert!(
        !legacy_lane.exists(),
        "legacy finish did not remove the worktree"
    );
    let worktrees = git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains(&format!("worktree {}", legacy_lane.display())));

    // A released generation may restart only when its old worktree is absent
    // and unregistered, while the issue's branch/worktree refs still identify
    // that exact lane. The surviving branch is attached as a fresh generation.
    let (restart_lane, restart_branch) = fx.start_lane("C84-4", "restart-generation");
    fx.commit_and_merge(
        &restart_lane,
        &restart_branch,
        "restart generation evidence",
    );
    let mut finish = fx.finish_command("C84-4", &restart_lane);
    finish
        .arg("--keep-branch")
        .env("CADENCE_TEST_PROC_ROOT", &fx.empty_proc_root);
    let finish = cadence_agent::reaper::output(&mut finish).expect("finish restart fixture");
    assert!(
        finish.status.success(),
        "finish failed: {}",
        output_text(&finish)
    );
    assert!(!restart_lane.exists());
    assert!(fx.branch_exists(&restart_branch));
    let released = lifecycle::managed_record(&fx.repo, &restart_lane)
        .expect("read released generation")
        .expect("released generation remains recorded");
    assert_eq!(released.state, "released");
    fx.assert_lane_refs("C84-4", &restart_lane, &restart_branch);
    let refs = fx.issue_refs("C84-4");
    assert_eq!(
        refs.iter()
            .find(|r| r.kind == "worktree"
                && r.path.as_deref() == Some(restart_lane.to_str().unwrap()))
            .unwrap()
            .closed,
        Some(true),
        "finish should close the removed worktree ref"
    );
    assert_ne!(
        refs.iter()
            .find(|r| r.kind == "branch" && r.path.as_deref() == Some(restart_branch.as_str()))
            .unwrap()
            .closed,
        Some(true),
        "kept branch ref should survive the release"
    );
    assert!(
        !git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"])
            .contains(&format!("worktree {}", restart_lane.display()))
    );

    let mut restart = fx.start_command("C84-4", "restart-generation");
    let restarted = cadence_agent::reaper::output(&mut restart).expect("restart released lane");
    assert!(
        restarted.status.success(),
        "restart failed: {}",
        output_text(&restarted)
    );
    assert!(
        restart_lane.is_dir(),
        "valid released generation was not reattached"
    );
    assert!(
        git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"])
            .contains(&format!("worktree {}", restart_lane.display()))
    );
    assert_eq!(
        lifecycle::managed_record(&fx.repo, &restart_lane)
            .expect("read restarted generation")
            .expect("new generation record exists")
            .state,
        "active"
    );

    // A released record cannot be restarted while its target still exists and
    // remains registered, even though its issue refs still match.
    let release = fx.release(&restart_lane, "leave released checkout for refusal");
    assert!(
        release.status.success(),
        "release failed: {}",
        output_text(&release)
    );
    let mut existing = fx.start_command("C84-4", "restart-generation");
    let existing = cadence_agent::reaper::output(&mut existing).expect("restart existing target");
    assert_refused(&existing, "still exists");
    assert!(restart_lane.is_dir());
    assert_eq!(
        lifecycle::managed_record(&fx.repo, &restart_lane)
            .expect("read released existing target")
            .expect("released record remains")
            .state,
        "released"
    );

    // Missing data is still not restartable while Git retains its registration.
    fs::remove_dir_all(&restart_lane).expect("remove released fixture data only");
    let registered = git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"]);
    assert!(registered.contains(&format!("worktree {}", restart_lane.display())));
    let mut registered_start = fx.start_command("C84-4", "restart-generation");
    let registered_start = cadence_agent::reaper::output(&mut registered_start)
        .expect("restart still-registered target");
    assert_refused(&registered_start, "registered by git");
    assert!(!restart_lane.exists());

    // After the stale registration is explicitly pruned, a mismatched branch
    // ref still cannot authorize a new lifecycle generation at the old path.
    git(
        &fx.home,
        &fx.repo,
        &["worktree", "prune", "--expire", "now"],
    );
    let pruned = git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"]);
    assert!(!pruned.contains(&format!("worktree {}", restart_lane.display())));
    fx.replace_branch_ref("C84-4", &restart_branch, "cadence/C84-4-wrong-disposition");
    let mut mismatched_start = fx.start_command("C84-4", "restart-generation");
    let mismatched_start = cadence_agent::reaper::output(&mut mismatched_start)
        .expect("restart mismatched disposition");
    assert_refused(&mismatched_start, "disposition");
    assert!(!restart_lane.exists());
    assert!(fx.branch_exists(&restart_branch));
    assert_eq!(
        lifecycle::managed_record(&fx.repo, &restart_lane)
            .expect("read mismatched released target")
            .expect("released record remains")
            .state,
        "released"
    );

    {
        let fx = Fixture::new();
        fx.new_issue("C84-5", "explicit resume acceptance");
        fx.new_issue("C84-6", "moved resume refusal");
        fx.new_issue("C84-7", "interrupted release recovery");
        fx.new_issue("C84-8", "canonical path alias refusal");
        fx.new_issue("C84-9", "missing retained checkout refusal");
        fx.new_issue("C84-10", "refs-only path reappearance refusal");
        fx.new_issue("C84-11", "reappearing checkout preservation");
        fx.new_issue("C84-12", "missing interrupted checkout recovery");

        let (resume_lane, resume_branch) = fx.start_lane("C84-5", "explicit-resume");
        fs::write(
            resume_lane.join("resume-history.txt"),
            "committed history\n",
        )
        .unwrap();
        git(&fx.home, &resume_lane, &["add", "resume-history.txt"]);
        git(
            &fx.home,
            &resume_lane,
            &["commit", "--quiet", "-m", "resume history"],
        );
        let sentinel = resume_lane.join("dirty-sentinel.txt");
        fs::write(&sentinel, "keep this dirty evidence\n").unwrap();

        let release_artifact = fx._root.path().join("resume-release-artifact.txt");
        let rollback_artifact = fx._root.path().join("resume-rollback-artifact.txt");
        fs::write(&release_artifact, "release artifact\n").unwrap();
        fs::write(&rollback_artifact, "rollback artifact\n").unwrap();
        let release_artifacts = vec![release_artifact.display().to_string()];
        let rollback_artifacts = vec![rollback_artifact.display().to_string()];
        lifecycle::declare_artifacts(
            &fx.repo,
            &resume_lane,
            release_artifacts.clone(),
            rollback_artifacts.clone(),
        )
        .expect("declare external recovery artifacts");

        let resume_head = git(&fx.home, &resume_lane, &["rev-parse", "HEAD"]);
        let resume_history = git(&fx.home, &resume_lane, &["rev-list", "--reverse", "HEAD"]);
        assert_eq!(resume_head.len(), 40);
        let retain_reason = "preserve explicit-resume lane for investigation";
        let identity = lifecycle::managed_record(&fx.repo, &resume_lane)
            .expect("read initial resume lifecycle record")
            .expect("resume record exists");
        let initial_pin = identity.pinned_sha.clone();
        assert_ne!(
            initial_pin, resume_head,
            "the pre-release commit must advance development HEAD beyond its initial pin"
        );
        let issue_refs = fx.issue_refs("C84-5");
        let ledger_file = fx.repo.join(".cadence/managed-checkouts.json");
        let ledger_bytes = || fs::read(&ledger_file).expect("read lifecycle ledger bytes");
        let assert_preserved = |expected_state: &str| {
            assert!(resume_lane.is_dir());
            assert!(fx.branch_exists(&resume_branch));
            assert!(
                git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"])
                    .contains(&format!("worktree {}", resume_lane.display()))
            );
            assert_eq!(fx.branch_tip(&resume_branch), resume_head);
            assert_eq!(
                git(&fx.home, &resume_lane, &["symbolic-ref", "--short", "HEAD"]),
                resume_branch
            );
            assert_eq!(
                git(&fx.home, &resume_lane, &["rev-parse", "HEAD"]),
                resume_head
            );
            assert_eq!(
                git(&fx.home, &resume_lane, &["rev-list", "--reverse", "HEAD"]),
                resume_history
            );
            assert_eq!(
                fs::read_to_string(&sentinel).unwrap(),
                "keep this dirty evidence\n"
            );
            assert_eq!(
                fs::read_to_string(&release_artifact).unwrap(),
                "release artifact\n"
            );
            assert_eq!(
                fs::read_to_string(&rollback_artifact).unwrap(),
                "rollback artifact\n"
            );
            assert_eq!(
                git(
                    &fx.home,
                    &resume_lane,
                    &[
                        "status",
                        "--porcelain",
                        "--untracked-files=all",
                        "--",
                        "dirty-sentinel.txt"
                    ],
                ),
                "?? dirty-sentinel.txt"
            );
            let current = lifecycle::managed_record(&fx.repo, &resume_lane)
                .expect("read resumed lifecycle record")
                .expect("resume record remains");
            assert_eq!(current.state, expected_state);
            let expected_retention_reason = match expected_state {
                "retained" | "released" => Some(retain_reason),
                "active" => None,
                state => panic!("unexpected lifecycle state in resume fixture: {state}"),
            };
            assert_eq!(
                current.retention_reason.as_deref(),
                expected_retention_reason
            );
            if matches!(expected_state, "retained" | "released") {
                assert_eq!(current.pinned_sha, initial_pin);
            }
            assert_eq!(current.repo, identity.repo);
            assert_eq!(current.path, identity.path);
            assert_eq!(current.purpose, identity.purpose);
            assert_eq!(current.tool, identity.tool);
            assert_eq!(current.owner, identity.owner);
            assert_eq!(current.branch, identity.branch);
            assert_eq!(current.issue, identity.issue);
            assert_eq!(current.base_sha, identity.base_sha);
            assert_eq!(current.release_artifacts, release_artifacts);
            assert_eq!(current.rollback_artifacts, rollback_artifacts);
            assert_eq!(fx.issue_refs("C84-5"), issue_refs);
        };

        let retained = fx.retain(&resume_lane, retain_reason);
        assert!(
            retained.status.success(),
            "retain failed: {}",
            output_text(&retained)
        );
        let retained_ledger = ledger_bytes();
        let retained_resume = fx.resume(&resume_lane, &resume_head, "reject retained resume");
        assert_refused(&retained_resume, "retained");
        assert_eq!(ledger_bytes(), retained_ledger);
        assert_preserved("retained");
        assert_eq!(
            lifecycle::managed_record(&fx.repo, &resume_lane)
                .expect("read retained resume record")
                .expect("retained record remains")
                .retention_reason
                .as_deref(),
            Some(retain_reason)
        );

        let unknown_lane = fx.repo.join(".cadence/wt/C84-5-unknown");
        let unknown_resume = fx.resume(&unknown_lane, &resume_head, "reject unknown resume path");
        assert_refused(&unknown_resume, "managed lifecycle record");
        assert!(!unknown_lane.exists());
        assert!(lifecycle::managed_record(&fx.repo, &unknown_lane)
            .expect("check unknown resume path")
            .is_none());
        assert_eq!(ledger_bytes(), retained_ledger);
        assert_preserved("retained");

        let released = fx.release(&resume_lane, "explicitly release before resume");
        assert!(
            released.status.success(),
            "release failed: {}",
            output_text(&released)
        );
        let released_ledger = ledger_bytes();
        assert_preserved("released");
        let released_record = lifecycle::managed_record(&fx.repo, &resume_lane)
            .expect("read released resume record")
            .expect("released record remains");
        assert_eq!(released_record.pinned_sha, initial_pin);
        assert_eq!(
            released_record.retention_reason.as_deref(),
            Some(retain_reason),
            "release alone must not clear the retention reason"
        );

        let noncanonical = resume_lane
            .parent()
            .expect("lane parent")
            .join("..")
            .join("wt")
            .join(resume_lane.file_name().expect("lane name"));
        assert_ne!(noncanonical.as_os_str(), resume_lane.as_os_str());
        assert_eq!(
            noncanonical.canonicalize().unwrap(),
            resume_lane.canonicalize().unwrap()
        );
        let noncanonical_resume =
            fx.resume(&noncanonical, &resume_head, "reject noncanonical path");
        assert_refused(&noncanonical_resume, "canonical");
        assert_eq!(ledger_bytes(), released_ledger);
        assert_preserved("released");

        let symlink_alias = fx._root.path().join("resume-symlink");
        std::os::unix::fs::symlink(&resume_lane, &symlink_alias).unwrap();
        let symlink_resume = fx.resume(&symlink_alias, &resume_head, "reject symlink path");
        assert_refused(&symlink_resume, "real checkout directory");
        assert_eq!(ledger_bytes(), released_ledger);
        assert_preserved("released");

        let wrong_pin = git(&fx.home, &resume_lane, &["rev-parse", "HEAD~1"]);
        assert_eq!(wrong_pin.len(), 40);
        assert_ne!(wrong_pin, resume_head);
        let wrong_resume = fx.resume(&resume_lane, &wrong_pin, "reject wrong SHA");
        assert_refused(&wrong_resume, "supplied pinned sha");
        assert_eq!(ledger_bytes(), released_ledger);
        assert_preserved("released");

        let resumed = fx.resume(
            &resume_lane,
            &resume_head,
            "resume exact released generation",
        );
        assert!(
            resumed.status.success(),
            "resume failed: {}",
            output_text(&resumed)
        );
        assert_preserved("active");
        let current = lifecycle::managed_record(&fx.repo, &resume_lane)
            .expect("read exact-head resumed record")
            .expect("resumed record remains");
        assert_eq!(
            current.pinned_sha, resume_head,
            "development pin must advance to the actual resumed HEAD"
        );
        assert!(current.retention_reason.is_none());

        let mut ordinary_start = fx.start_command("C84-5", "explicit-resume");
        let started = cadence_agent::reaper::output(&mut ordinary_start)
            .expect("ordinary issue start after explicit resume");
        assert!(
            started.status.success(),
            "start failed: {}",
            output_text(&started)
        );
        assert_preserved("active");

        let (moved_lane, moved_branch) = fx.start_lane("C84-6", "moved-resume");
        fs::write(
            moved_lane.join("moved-history.txt"),
            "preserve moved checkout\n",
        )
        .unwrap();
        git(&fx.home, &moved_lane, &["add", "moved-history.txt"]);
        git(
            &fx.home,
            &moved_lane,
            &["commit", "--quiet", "-m", "moved checkout history"],
        );
        let moved_head = git(&fx.home, &moved_lane, &["rev-parse", "HEAD"]);
        let moved_history = git(&fx.home, &moved_lane, &["rev-list", "--reverse", "HEAD"]);
        let moved_issue_refs = fx.issue_refs("C84-6");
        let moved_release = fx.release(&moved_lane, "release before path identity checks");
        assert!(
            moved_release.status.success(),
            "release failed: {}",
            output_text(&moved_release)
        );
        let moved_ledger = ledger_bytes();
        let relocated = moved_lane.with_file_name("C84-6-relocated");
        git(
            &fx.home,
            &fx.repo,
            &[
                "worktree",
                "move",
                moved_lane.to_str().expect("original worktree path"),
                relocated.to_str().expect("relocated worktree path"),
            ],
        );
        let moved_resume = fx.resume(&relocated, &moved_head, "reject moved lifecycle path");
        assert_refused(&moved_resume, "managed lifecycle record");
        assert_eq!(ledger_bytes(), moved_ledger);
        assert_eq!(
            fs::read_to_string(relocated.join("moved-history.txt")).unwrap(),
            "preserve moved checkout\n"
        );
        assert_eq!(fx.branch_tip(&moved_branch), moved_head);
        assert_eq!(
            git(
                &fx.home,
                &fx.repo,
                &["rev-list", "--reverse", &moved_branch]
            ),
            moved_history
        );
        assert_eq!(fx.issue_refs("C84-6"), moved_issue_refs);

        // Put the directory back without repairing Git's moved registration:
        // resume must refuse a real linked checkout absent from Git's live list.
        fs::rename(&relocated, &moved_lane).expect("restore fixture directory at recorded path");
        let registered = git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"]);
        assert!(registered.contains(&format!("worktree {}", relocated.display())));
        assert!(!registered.contains(&format!("worktree {}", moved_lane.display())));
        let unregistered_resume =
            fx.resume(&moved_lane, &moved_head, "reject unregistered worktree");
        assert_refused(&unregistered_resume, "registered exactly once");
        assert_eq!(ledger_bytes(), moved_ledger);
        assert_eq!(
            fs::read_to_string(moved_lane.join("moved-history.txt")).unwrap(),
            "preserve moved checkout\n"
        );
        assert_eq!(fx.branch_tip(&moved_branch), moved_head);
        assert_eq!(fx.issue_refs("C84-6"), moved_issue_refs);

        let (interrupted_lane, interrupted_branch) = fx.start_lane("C84-7", "interrupted-release");
        fs::write(
            interrupted_lane.join("interrupted-history.txt"),
            "preserve interrupted checkout\n",
        )
        .unwrap();
        git(
            &fx.home,
            &interrupted_lane,
            &["add", "interrupted-history.txt"],
        );
        git(
            &fx.home,
            &interrupted_lane,
            &["commit", "--quiet", "-m", "interrupted release history"],
        );
        let interrupted_head = git(&fx.home, &interrupted_lane, &["rev-parse", "HEAD"]);
        let interrupted_history = git(
            &fx.home,
            &interrupted_lane,
            &["rev-list", "--reverse", "HEAD"],
        );
        let interrupted_identity = lifecycle::managed_record(&fx.repo, &interrupted_lane)
            .expect("read interrupted release identity")
            .expect("interrupted release record exists");
        let interrupted_refs = fx.issue_refs("C84-7");
        let guard = lifecycle::begin_release(
            &fx.repo,
            &interrupted_lane,
            "C84-7",
            Some(&interrupted_branch),
            "simulate a process interrupted during release",
        )
        .expect("begin real guarded release transaction");
        assert!(lifecycle_lock_is_held(&fx.repo).expect("probe live release lock"));

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let resume_repo = fx.repo.clone();
        let resume_path = interrupted_lane.clone();
        let resume_pin = interrupted_head.clone();
        let waiting_resume = thread::spawn(move || {
            started_tx.send(()).expect("signal resume thread start");
            result_tx
                .send(
                    lifecycle::resume(
                        &resume_repo,
                        &resume_path,
                        &resume_pin,
                        "resume cannot cross live release lock",
                    )
                    .map_err(|error| error.to_string()),
                )
                .expect("send resume result");
        });
        started_rx
            .recv()
            .expect("resume thread starts while release lock is held");
        assert!(
            matches!(
                result_rx.recv_timeout(Duration::from_millis(500)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "resume returned while the release transaction still held its lock"
        );
        drop(guard);
        let resume_error = result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("resume completes after interrupted lock is released")
            .expect_err("resume must reject interrupted releasing metadata");
        assert!(
            resume_error.contains("interrupted release"),
            "unexpected interrupted resume refusal: {resume_error}"
        );
        waiting_resume.join().expect("join resumed release waiter");
        let releasing = lifecycle::managed_record(&fx.repo, &interrupted_lane)
            .expect("read interrupted release record")
            .expect("interrupted record remains");
        assert_eq!(releasing.state, "releasing");
        assert!(
            !lifecycle_lock_is_held(&fx.repo).expect("probe dropped release lock"),
            "release recovery must run only after the live guard is gone"
        );
        let interrupted_ledger = ledger_bytes();

        let generic_retain = fx.retain(
            &interrupted_lane,
            "generic retention cannot resolve interrupted release",
        );
        assert_refused(&generic_retain, "release-interrupted");
        assert_eq!(ledger_bytes(), interrupted_ledger);
        let generic_activate = lifecycle::transition(
            &fx.repo,
            &interrupted_lane,
            "active",
            Some("generic activation cannot resolve interrupted release"),
        )
        .expect_err("generic active transition must not recover interrupted release")
        .to_string();
        assert!(
            generic_activate.contains("release-interrupted"),
            "unexpected generic activation refusal: {generic_activate}"
        );
        assert_eq!(ledger_bytes(), interrupted_ledger);
        let empty_release = fx.release(&interrupted_lane, "  ");
        assert_refused(&empty_release, "requires a reason");
        assert_eq!(ledger_bytes(), interrupted_ledger);
        assert_eq!(
            lifecycle::managed_record(&fx.repo, &interrupted_lane)
                .expect("read after refused generic mutations")
                .expect("interrupted record remains")
                .state,
            "releasing"
        );

        let recovery_reason = "acknowledge interrupted release after inspection";
        let recovered = fx.release(&interrupted_lane, recovery_reason);
        assert!(
            recovered.status.success(),
            "reasoned release recovery failed: {}",
            output_text(&recovered)
        );
        let released = lifecycle::managed_record(&fx.repo, &interrupted_lane)
            .expect("read reasoned release recovery")
            .expect("recovered record remains");
        assert_eq!(released.state, "released");
        assert_eq!(released.release_reason.as_deref(), Some(recovery_reason));
        assert_eq!(released.repo, interrupted_identity.repo);
        assert_eq!(released.path, interrupted_identity.path);
        assert_eq!(released.purpose, interrupted_identity.purpose);
        assert_eq!(released.tool, interrupted_identity.tool);
        assert_eq!(released.owner, interrupted_identity.owner);
        assert_eq!(released.branch, interrupted_identity.branch);
        assert_eq!(released.issue, interrupted_identity.issue);
        assert_eq!(released.base_sha, interrupted_identity.base_sha);
        assert_eq!(fx.issue_refs("C84-7"), interrupted_refs);
        assert!(interrupted_lane.is_dir());
        assert_eq!(fx.branch_tip(&interrupted_branch), interrupted_head);
        assert_eq!(
            git(
                &fx.home,
                &interrupted_lane,
                &["rev-list", "--reverse", "HEAD"]
            ),
            interrupted_history
        );
        assert_eq!(
            fs::read_to_string(interrupted_lane.join("interrupted-history.txt")).unwrap(),
            "preserve interrupted checkout\n"
        );
        let resumed = fx.resume(
            &interrupted_lane,
            &interrupted_head,
            "resume after reasoned interrupted-release recovery",
        );
        assert!(
            resumed.status.success(),
            "resume after release recovery failed: {}",
            output_text(&resumed)
        );
        assert_eq!(
            lifecycle::managed_record(&fx.repo, &interrupted_lane)
                .expect("read resumed interrupted-release record")
                .expect("resumed record remains")
                .state,
            "active"
        );
        assert_eq!(fx.issue_refs("C84-7"), interrupted_refs);

        {
            let (interrupted_lane, interrupted_branch) =
                fx.start_lane("C84-12", "interrupted-release-missing-path");
            fs::write(
                interrupted_lane.join("interrupted-history.txt"),
                "preserve interrupted checkout\n",
            )
            .unwrap();
            git(
                &fx.home,
                &interrupted_lane,
                &["add", "interrupted-history.txt"],
            );
            git(
                &fx.home,
                &interrupted_lane,
                &["commit", "--quiet", "-m", "interrupted release history"],
            );
            let interrupted_head = git(&fx.home, &interrupted_lane, &["rev-parse", "HEAD"]);
            let interrupted_history = git(
                &fx.home,
                &interrupted_lane,
                &["rev-list", "--reverse", "HEAD"],
            );
            let release_artifact = fx._root.path().join("C84-12-release-artifact.txt");
            let rollback_artifact = fx._root.path().join("C84-12-rollback-artifact.txt");
            fs::write(&release_artifact, "release artifact survives\n").unwrap();
            fs::write(&rollback_artifact, "rollback artifact survives\n").unwrap();
            let release_artifacts = vec![release_artifact.display().to_string()];
            let rollback_artifacts = vec![rollback_artifact.display().to_string()];
            lifecycle::declare_artifacts(
                &fx.repo,
                &interrupted_lane,
                release_artifacts.clone(),
                rollback_artifacts.clone(),
            )
            .expect("declare external recovery artifacts");
            let interrupted_identity = lifecycle::managed_record(&fx.repo, &interrupted_lane)
                .expect("read interrupted release identity")
                .expect("interrupted release record exists");
            let interrupted_refs = fx.issue_refs("C84-12");
            let guard = lifecycle::begin_release(
                &fx.repo,
                &interrupted_lane,
                "C84-12",
                Some(&interrupted_branch),
                "simulate a process interrupted during release",
            )
            .expect("begin real guarded release transaction");
            assert!(lifecycle_lock_is_held(&fx.repo).expect("probe live release lock"));

            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            let resume_repo = fx.repo.clone();
            let resume_path = interrupted_lane.clone();
            let resume_pin = interrupted_head.clone();
            let waiting_resume = thread::spawn(move || {
                started_tx.send(()).expect("signal resume thread start");
                result_tx
                    .send(
                        lifecycle::resume(
                            &resume_repo,
                            &resume_path,
                            &resume_pin,
                            "resume cannot cross live release lock",
                        )
                        .map_err(|error| error.to_string()),
                    )
                    .expect("send resume result");
            });
            started_rx
                .recv()
                .expect("resume thread starts while release lock is held");
            assert!(
                matches!(
                    result_rx.recv_timeout(Duration::from_millis(500)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ),
                "resume returned while the release transaction still held its lock"
            );
            git(
                &fx.home,
                &fx.repo,
                &[
                    "worktree",
                    "remove",
                    "--",
                    interrupted_lane
                        .to_str()
                        .expect("interrupted worktree path"),
                ],
            );
            assert!(!interrupted_lane.exists());
            assert!(fx.branch_exists(&interrupted_branch));
            assert_eq!(fx.branch_tip(&interrupted_branch), interrupted_head);
            assert_eq!(
                git(
                    &fx.home,
                    &fx.repo,
                    &["rev-list", "--reverse", &interrupted_branch]
                ),
                interrupted_history
            );
            drop(guard);
            let resume_error = result_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("resume completes after interrupted lock is released")
                .expect_err("resume must reject interrupted releasing metadata");
            assert!(
                resume_error.contains("interrupted release"),
                "unexpected interrupted resume refusal: {resume_error}"
            );
            waiting_resume.join().expect("join resumed release waiter");
            let releasing = lifecycle::managed_record(&fx.repo, &interrupted_lane)
                .expect("read interrupted release record")
                .expect("interrupted record remains");
            assert_eq!(releasing.state, "releasing");
            assert!(
                !lifecycle_lock_is_held(&fx.repo).expect("probe dropped release lock"),
                "release recovery must run only after the live guard is gone"
            );
            let interrupted_ledger = ledger_bytes();
            let issue_file = fx.pm.dir.join(PROJECT).join("C84-12/issue.md");
            let interrupted_issue_bytes = fs::read(&issue_file).expect("snapshot interrupted refs");
            let tracker_head = git(&fx.home, &fx.pm.dir, &["rev-parse", "HEAD"]);
            assert_eq!(fx.issue_refs("C84-12"), interrupted_refs);
            assert!(!interrupted_lane.exists());
            assert!(
                !git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"])
                    .contains(&format!("worktree {}", interrupted_lane.display())),
                "the crash fixture must have removed the Git worktree registration"
            );
            assert_eq!(
                releasing.release_reason.as_deref(),
                Some("simulate a process interrupted during release")
            );

            let mut finish = fx.finish_command("C84-12", &interrupted_lane);
            let finish = cadence_agent::reaper::output(&mut finish)
                .expect("run ordinary finish against missing interrupted checkout");
            assert_refused(&finish, "release");
            assert_eq!(ledger_bytes(), interrupted_ledger);
            assert_eq!(fs::read(&issue_file).unwrap(), interrupted_issue_bytes);
            assert_eq!(
                git(&fx.home, &fx.pm.dir, &["rev-parse", "HEAD"]),
                tracker_head,
                "refusal must not add tracker history"
            );
            assert_eq!(fx.issue_refs("C84-12"), interrupted_refs);
            assert_eq!(fx.branch_tip(&interrupted_branch), interrupted_head);
            assert_eq!(
                git(
                    &fx.home,
                    &fx.repo,
                    &["rev-list", "--reverse", &interrupted_branch]
                ),
                interrupted_history
            );
            assert_eq!(
                fs::read(&release_artifact).unwrap(),
                b"release artifact survives\n"
            );
            assert_eq!(
                fs::read(&rollback_artifact).unwrap(),
                b"rollback artifact survives\n"
            );
            assert!(!interrupted_lane.exists());

            let generic_release = lifecycle::transition(
                &fx.repo,
                &interrupted_lane,
                "released",
                Some("generic transition cannot resolve interrupted release"),
            )
            .expect_err("generic transition must not finalize interrupted release")
            .to_string();
            assert!(
                generic_release.contains("release") || generic_release.contains("releasing"),
                "unexpected generic release refusal: {generic_release}"
            );
            assert_eq!(ledger_bytes(), interrupted_ledger);
            let generic_retain = fx.retain(
                &interrupted_lane,
                "generic retention cannot resolve interrupted release",
            );
            assert_refused(&generic_retain, "release-interrupted");
            assert_eq!(ledger_bytes(), interrupted_ledger);
            let generic_activate = lifecycle::transition(
                &fx.repo,
                &interrupted_lane,
                "active",
                Some("generic activation cannot resolve interrupted release"),
            )
            .expect_err("generic active transition must not recover interrupted release")
            .to_string();
            assert!(
                generic_activate.contains("release-interrupted"),
                "unexpected generic activation refusal: {generic_activate}"
            );
            assert_eq!(ledger_bytes(), interrupted_ledger);
            let empty_release = fx.release(&interrupted_lane, "  ");
            assert_refused(&empty_release, "requires a reason");
            assert_eq!(ledger_bytes(), interrupted_ledger);
            assert_eq!(
                lifecycle::managed_record(&fx.repo, &interrupted_lane)
                    .expect("read after refused generic mutations")
                    .expect("interrupted record remains")
                    .state,
                "releasing"
            );

            let recovery_reason = "inspected missing checkout; preserve surviving branch";
            let recovered = fx.release(&interrupted_lane, recovery_reason);
            assert!(
                recovered.status.success(),
                "reasoned release recovery failed: {}",
                output_text(&recovered)
            );
            let released = lifecycle::managed_record(&fx.repo, &interrupted_lane)
                .expect("read reasoned release recovery")
                .expect("recovered record remains");
            assert_eq!(released.state, "released");
            assert_eq!(released.release_reason.as_deref(), Some(recovery_reason));
            assert_eq!(released.repo, interrupted_identity.repo);
            assert_eq!(released.path, interrupted_identity.path);
            assert_eq!(released.purpose, interrupted_identity.purpose);
            assert_eq!(released.tool, interrupted_identity.tool);
            assert_eq!(released.owner, interrupted_identity.owner);
            assert_eq!(released.branch, interrupted_identity.branch);
            assert_eq!(released.issue, interrupted_identity.issue);
            assert_eq!(released.base_sha, interrupted_identity.base_sha);
            assert_eq!(released.release_artifacts, release_artifacts);
            assert_eq!(released.rollback_artifacts, rollback_artifacts);
            assert_eq!(fx.issue_refs("C84-12"), interrupted_refs);
            assert!(!interrupted_lane.exists());
            assert!(fx.branch_exists(&interrupted_branch));
            assert_eq!(fx.branch_tip(&interrupted_branch), interrupted_head);
            assert_eq!(
                git(
                    &fx.home,
                    &fx.repo,
                    &["rev-list", "--reverse", &interrupted_branch]
                ),
                interrupted_history
            );
            assert_eq!(
                fs::read(&release_artifact).unwrap(),
                b"release artifact survives\n"
            );
            assert_eq!(
                fs::read(&rollback_artifact).unwrap(),
                b"rollback artifact survives\n"
            );
            assert_eq!(fs::read(&issue_file).unwrap(), interrupted_issue_bytes);
            assert_eq!(
                git(&fx.home, &fx.pm.dir, &["rev-parse", "HEAD"]),
                tracker_head,
                "explicit recovery must not mutate tracker history"
            );

            let mut finish = fx.finish_command("C84-12", &interrupted_lane);
            let finish = cadence_agent::reaper::output(&mut finish)
                .expect("close the explicitly recovered missing worktree ref");
            assert!(
                finish.status.success(),
                "refs-only closure failed: {}",
                output_text(&finish)
            );
            let closed_refs = fx.issue_refs("C84-12");
            assert_eq!(
                closed_refs
                    .iter()
                    .find(|r| r.kind == "worktree")
                    .expect("worktree ref remains as history")
                    .closed,
                Some(true)
            );
            assert_ne!(
                closed_refs
                    .iter()
                    .find(|r| r.kind == "branch")
                    .expect("branch ref remains open")
                    .closed,
                Some(true)
            );
            assert!(fx.branch_exists(&interrupted_branch));
            assert_eq!(fx.branch_tip(&interrupted_branch), interrupted_head);
            assert_eq!(
                git(
                    &fx.home,
                    &fx.repo,
                    &["rev-list", "--reverse", &interrupted_branch]
                ),
                interrupted_history
            );
            assert_eq!(
                fs::read(&release_artifact).unwrap(),
                b"release artifact survives\n"
            );
            assert_eq!(
                fs::read(&rollback_artifact).unwrap(),
                b"rollback artifact survives\n"
            );
            let closed_record = lifecycle::managed_record(&fx.repo, &interrupted_lane)
                .expect("read released refs-only lifecycle record")
                .expect("released record remains");
            assert_eq!(closed_record.state, "released");
            assert_eq!(
                closed_record.release_reason.as_deref(),
                Some(recovery_reason)
            );
            assert_eq!(closed_record.release_artifacts, release_artifacts);
            assert_eq!(closed_record.rollback_artifacts, rollback_artifacts);
            assert!(!interrupted_lane.exists());
        }

        let (retained_missing, retained_branch) = fx.start_lane("C84-9", "missing-retained");
        fs::write(
            retained_missing.join("retained-history.txt"),
            "preserve retained branch history\n",
        )
        .unwrap();
        git(
            &fx.home,
            &retained_missing,
            &["add", "retained-history.txt"],
        );
        git(
            &fx.home,
            &retained_missing,
            &["commit", "--quiet", "-m", "retained branch history"],
        );
        let retained_head = git(&fx.home, &retained_missing, &["rev-parse", "HEAD"]);
        let retained_history = git(
            &fx.home,
            &retained_missing,
            &["rev-list", "--reverse", "HEAD"],
        );
        let retained_reason = "retain missing lane pending explicit recovery";
        let retained = fx.retain(&retained_missing, retained_reason);
        assert!(
            retained.status.success(),
            "retain failed: {}",
            output_text(&retained)
        );
        git(
            &fx.home,
            &fx.repo,
            &[
                "worktree",
                "remove",
                "--",
                retained_missing.to_str().expect("retained worktree path"),
            ],
        );
        assert!(!retained_missing.exists());
        let retained_missing_ledger = ledger_bytes();
        let retained_issue_file = fx.pm.dir.join(PROJECT).join("C84-9/issue.md");
        let retained_issue_bytes = fs::read(&retained_issue_file).unwrap();
        let retained_tracker_head = git(&fx.home, &fx.pm.dir, &["rev-parse", "HEAD"]);
        let retained_refs = fx.issue_refs("C84-9");
        let mut finish = fx.finish_command("C84-9", &retained_missing);
        let finish = cadence_agent::reaper::output(&mut finish)
            .expect("run refs-only finish against missing retained checkout");
        assert_refused(&finish, "retained");
        assert_eq!(ledger_bytes(), retained_missing_ledger);
        assert_eq!(
            fs::read(&retained_issue_file).unwrap(),
            retained_issue_bytes
        );
        assert_eq!(
            git(&fx.home, &fx.pm.dir, &["rev-parse", "HEAD"]),
            retained_tracker_head
        );
        assert_eq!(fx.issue_refs("C84-9"), retained_refs);
        assert_eq!(fx.branch_tip(&retained_branch), retained_head);
        assert_eq!(
            git(
                &fx.home,
                &fx.repo,
                &["rev-list", "--reverse", &retained_branch]
            ),
            retained_history
        );
        let retained_record = lifecycle::managed_record(&fx.repo, &retained_missing)
            .expect("read missing retained record")
            .expect("retained record remains");
        assert_eq!(retained_record.state, "retained");
        assert_eq!(
            retained_record.retention_reason.as_deref(),
            Some(retained_reason)
        );
        assert!(!retained_missing.exists());
        assert!(fx.branch_exists(&retained_branch));

        let (gone_lane, gone_branch) = fx.start_lane("C84-10", "c84-10");
        fs::write(
            gone_lane.join("gone-history.txt"),
            "original lane history\n",
        )
        .unwrap();
        git(&fx.home, &gone_lane, &["add", "gone-history.txt"]);
        git(
            &fx.home,
            &gone_lane,
            &["commit", "--quiet", "-m", "original lane history"],
        );
        let gone_head = git(&fx.home, &gone_lane, &["rev-parse", "HEAD"]);
        let gone_history = git(&fx.home, &gone_lane, &["rev-list", "--reverse", "HEAD"]);
        git(
            &fx.home,
            &fx.repo,
            &[
                "worktree",
                "remove",
                "--",
                gone_lane.to_str().expect("initially missing worktree path"),
            ],
        );
        assert!(!gone_lane.exists());
        let gone_refs = fx.issue_refs("C84-10");

        let (reappearing_lane, reappearing_branch) = fx.start_lane("C84-11", "c84-11");
        fs::write(
            reappearing_lane.join("reappearing-history.txt"),
            "other linked checkout survives\n",
        )
        .unwrap();
        git(
            &fx.home,
            &reappearing_lane,
            &["add", "reappearing-history.txt"],
        );
        git(
            &fx.home,
            &reappearing_lane,
            &["commit", "--quiet", "-m", "other linked checkout history"],
        );
        let reappearing_head = git(&fx.home, &reappearing_lane, &["rev-parse", "HEAD"]);
        let reappearing_history = git(
            &fx.home,
            &reappearing_lane,
            &["rev-list", "--reverse", "HEAD"],
        );
        let reappearing_refs = fx.issue_refs("C84-11");
        let reappearing_issue_file = fx.pm.dir.join(PROJECT).join("C84-11/issue.md");
        let reappearing_issue_bytes = fs::read(&reappearing_issue_file).unwrap();
        let observer = GitObserver::at_first_matching_git_arg(
            &fx,
            "refs-only-reappearance",
            format!("refs/heads/{gone_branch}"),
        );
        let mut finish = fx.finish_command("C84-10", &gone_lane);
        finish.arg("--force");
        observer.configure(&mut finish);
        let finish_child =
            cadence_agent::reaper::spawn(&mut finish).expect("spawn reappearance finish CLI");
        let mut race = RaceChildren {
            finish: Some(finish_child),
            retain: None,
            adopt: None,
            gate: observer.gate.clone(),
        };
        wait_for_file(&observer.entered, race.finish.as_mut().unwrap())
            .expect("finish has resolved the target path as missing");
        assert!(
            !observer.events.exists(),
            "finish reached Git worktree removal before the path reappeared"
        );
        assert!(!gone_lane.exists());
        git(
            &fx.home,
            &fx.repo,
            &[
                "worktree",
                "move",
                reappearing_lane.to_str().expect("other worktree path"),
                gone_lane.to_str().expect("reappearing target path"),
            ],
        );
        assert!(!reappearing_lane.exists());
        assert!(gone_lane.is_dir());
        assert_eq!(
            fs::read_to_string(gone_lane.join("reappearing-history.txt")).unwrap(),
            "other linked checkout survives\n"
        );
        let expected_registration = git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"]);
        assert!(expected_registration.contains(&format!("worktree {}", gone_lane.display())));
        assert!(expected_registration.contains(&format!("branch refs/heads/{reappearing_branch}")));
        let reappearance_ledger = ledger_bytes();
        let gone_issue_file = fx.pm.dir.join(PROJECT).join("C84-10/issue.md");
        let gone_issue_bytes = fs::read(&gone_issue_file).unwrap();
        let reappearance_tracker_head = git(&fx.home, &fx.pm.dir, &["rev-parse", "HEAD"]);
        fs::write(&observer.gate, "continue\n").expect("release Git probe barrier");
        let finish = race
            .finish
            .take()
            .unwrap()
            .wait_with_output()
            .expect("wait for refs-only finish after path reappearance");
        assert_refused(&finish, "reappeared");
        assert!(
            !observer.events.exists(),
            "refs-only finish attempted to remove a reappeared registered checkout"
        );
        assert_eq!(ledger_bytes(), reappearance_ledger);
        assert_eq!(fs::read(&gone_issue_file).unwrap(), gone_issue_bytes);
        assert_eq!(
            fs::read(&reappearing_issue_file).unwrap(),
            reappearing_issue_bytes
        );
        assert_eq!(
            git(&fx.home, &fx.pm.dir, &["rev-parse", "HEAD"]),
            reappearance_tracker_head
        );
        assert_eq!(fx.issue_refs("C84-10"), gone_refs);
        assert_eq!(fx.issue_refs("C84-11"), reappearing_refs);
        assert_eq!(fx.branch_tip(&gone_branch), gone_head);
        assert_eq!(
            git(&fx.home, &fx.repo, &["rev-list", "--reverse", &gone_branch]),
            gone_history
        );
        assert_eq!(fx.branch_tip(&reappearing_branch), reappearing_head);
        assert_eq!(
            git(
                &fx.home,
                &fx.repo,
                &["rev-list", "--reverse", &reappearing_branch]
            ),
            reappearing_history
        );
        assert_eq!(
            fs::read_to_string(gone_lane.join("reappearing-history.txt")).unwrap(),
            "other linked checkout survives\n"
        );
        assert_eq!(
            git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"]),
            expected_registration
        );
        drop(race);

        let (alias_lane, alias_branch) = fx.start_lane("C84-8", "alias-owner");
        fs::write(
            alias_lane.join("alias-history.txt"),
            "preserve aliased worktree history\n",
        )
        .unwrap();
        git(&fx.home, &alias_lane, &["add", "alias-history.txt"]);
        git(
            &fx.home,
            &alias_lane,
            &["commit", "--quiet", "-m", "alias owner history"],
        );
        let alias_head = git(&fx.home, &alias_lane, &["rev-parse", "HEAD"]);
        let alias_history = git(&fx.home, &alias_lane, &["rev-list", "--reverse", "HEAD"]);
        let alias_identity = lifecycle::managed_record(&fx.repo, &alias_lane)
            .expect("read alias-owner lifecycle record")
            .expect("alias-owner record exists");
        assert_eq!(alias_identity.state, "active");
        let alias_refs = fx.issue_refs("C84-8");

        let relocated_alias_lane = alias_lane.with_file_name("C84-8-relocated");
        git(
            &fx.home,
            &fx.repo,
            &[
                "worktree",
                "move",
                alias_lane.to_str().expect("original alias-owner path"),
                relocated_alias_lane
                    .to_str()
                    .expect("relocated alias-owner path"),
            ],
        );
        let moved_sentinel = relocated_alias_lane.join("moved-source-sentinel.txt");
        fs::write(&moved_sentinel, "preserve moved source sentinel\n").unwrap();
        std::os::unix::fs::symlink(&relocated_alias_lane, &alias_lane)
            .expect("alias old path to moved managed checkout");

        let alias_ledger = ledger_bytes();
        assert_eq!(alias_identity.path, alias_lane.display().to_string());
        let alias_registration = git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"]);
        assert!(
            alias_registration.contains(&format!("worktree {}", relocated_alias_lane.display()))
        );
        assert!(!alias_registration.contains(&format!("worktree {}", alias_lane.display())));
        assert_eq!(fx.branch_tip(&alias_branch), alias_head);
        assert_eq!(
            git(&fx.home, &relocated_alias_lane, &["rev-parse", "HEAD"]),
            alias_head
        );
        assert_eq!(
            git(
                &fx.home,
                &relocated_alias_lane,
                &["rev-list", "--reverse", "HEAD"]
            ),
            alias_history
        );

        let getter_error = lifecycle::managed_record(&fx.repo, &relocated_alias_lane)
            .expect_err("canonical alias must not resolve a differently recorded path");
        assert!(
            getter_error
                .to_string()
                .to_ascii_lowercase()
                .contains("path"),
            "unexpected managed_record alias refusal: {getter_error}"
        );
        assert_eq!(ledger_bytes(), alias_ledger);

        let release_error = match lifecycle::begin_release(
            &fx.repo,
            &relocated_alias_lane,
            "C84-8",
            Some(&alias_branch),
            "reject release through canonical alias",
        ) {
            Err(error) => error.to_string(),
            Ok(guard) => {
                drop(guard);
                panic!("begin_release accepted a path alias for another recorded path")
            }
        };
        assert!(
            release_error.to_ascii_lowercase().contains("path"),
            "unexpected begin_release alias refusal: {release_error}"
        );
        assert_eq!(ledger_bytes(), alias_ledger);
        assert_eq!(
            fs::read_to_string(&moved_sentinel).unwrap(),
            "preserve moved source sentinel\n"
        );
        assert_eq!(
            fs::read_to_string(relocated_alias_lane.join("alias-history.txt")).unwrap(),
            "preserve aliased worktree history\n"
        );
        assert_eq!(fx.branch_tip(&alias_branch), alias_head);
        assert_eq!(
            git(&fx.home, &relocated_alias_lane, &["rev-parse", "HEAD"]),
            alias_head
        );
        assert_eq!(
            git(
                &fx.home,
                &relocated_alias_lane,
                &["rev-list", "--reverse", "HEAD"]
            ),
            alias_history
        );
        assert_eq!(
            git(&fx.home, &fx.repo, &["worktree", "list", "--porcelain"]),
            alias_registration
        );
        assert_eq!(fx.issue_refs("C84-8"), alias_refs);
        assert!(fs::symlink_metadata(&alias_lane)
            .expect("old path remains the intended symlink")
            .file_type()
            .is_symlink());
    }
}

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    state: PathBuf,
    empty_proc_root: PathBuf,
    pm: Pm,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("c84ret-")
            .tempdir_in("/tmp")
            .expect("isolated fixture root");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.path().join("h");
        let state = root.path().join("s");
        let repo = root.path().join("repo");
        let empty_proc_root = root.path().join("proc");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(empty_proc_root.join("self")).unwrap();
        let canonical_proc_root = empty_proc_root.canonicalize().unwrap();
        fs::write(
            empty_proc_root.join("self/mountinfo"),
            format!(
                "1 0 0:1 / {} rw - proc proc rw\n",
                canonical_proc_root.display()
            ),
        )
        .unwrap();
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
            empty_proc_root,
            pm,
            repo,
        };
        fx.new_issue("C84-1", "retained lane fixture");
        fx.new_issue("C84-2", "release race fixture");
        fx.new_issue("C84-3", "legacy release race fixture");
        fx.new_issue("C84-4", "released generation boundary fixture");
        fx
    }

    fn new_issue(&self, id: &str, title: &str) {
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
            Some(id),
            Some("Isolated CAD-848 retention fixture."),
            ACTOR,
        )
        .expect("fixture issue");
    }

    fn start_lane(&self, id: &str, name: &str) -> (PathBuf, String) {
        let result = start::run(
            &self.pm,
            id,
            &start::StartArgs {
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
        .expect("create real managed issue lane");
        (
            PathBuf::from(result["worktree"].as_str().unwrap()),
            result["branch"].as_str().unwrap().to_string(),
        )
    }

    fn commit_and_merge(&self, lane: &Path, branch: &str, contents: &str) {
        fs::write(lane.join("retained-evidence.txt"), format!("{contents}\n")).unwrap();
        git(&self.home, lane, &["add", "retained-evidence.txt"]);
        git(&self.home, lane, &["commit", "--quiet", "-m", contents]);
        git(
            &self.home,
            &self.repo,
            &["merge", "--quiet", "--ff-only", branch],
        );
        git(
            &self.home,
            &self.repo,
            &["push", "--quiet", "origin", "main"],
        );
        git(&self.home, &self.repo, &["fetch", "--quiet", "origin"]);
        age_tracked_files(lane);
    }

    fn branch_tip(&self, branch: &str) -> String {
        let reference = format!("refs/heads/{branch}");
        git(&self.home, &self.repo, &["rev-parse", &reference])
    }

    fn branch_exists(&self, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        let mut cmd = Command::new(find_program("git").expect("Git on PATH"));
        cmd.args(["rev-parse", "--verify", "--quiet", &reference])
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        cadence_agent::reaper::output(&mut cmd)
            .expect("check fixture branch")
            .status
            .success()
    }

    fn issue_refs(&self, id: &str) -> Vec<Ref> {
        let file = self.pm.dir.join(PROJECT).join(id).join("issue.md");
        let text = fs::read_to_string(file).unwrap();
        parse::parse_issue(&text).unwrap().0.refs
    }

    fn replace_branch_ref(&self, id: &str, old: &str, new: &str) {
        let file = self.pm.dir.join(PROJECT).join(id).join("issue.md");
        let text = fs::read_to_string(&file).unwrap();
        let (mut front, body) = parse::parse_issue(&text).unwrap();
        let reference = front
            .refs
            .iter_mut()
            .find(|r| r.kind == "branch" && r.path.as_deref() == Some(old))
            .expect("matching branch ref exists");
        reference.path = Some(new.to_string());
        fs::write(&file, parse::render(&front, &body).unwrap()).unwrap();
    }

    fn assert_lane_refs(&self, id: &str, lane: &Path, branch: &str) {
        let refs = self.issue_refs(id);
        assert!(refs.iter().any(|r| {
            r.kind == "worktree" && r.path.as_deref() == Some(lane.to_string_lossy().as_ref())
        }));
        assert!(refs
            .iter()
            .any(|r| r.kind == "branch" && r.path.as_deref() == Some(branch)));
    }

    fn start_command(&self, id: &str, name: &str) -> Command {
        let mut cmd = self.cli();
        cmd.args([
            "issue",
            "start",
            id,
            "--repo",
            self.repo.to_str().unwrap(),
            "--name",
            name,
            "--owner",
            ACTOR,
            "--by",
            ACTOR,
        ]);
        cmd
    }

    fn release(&self, lane: &Path, reason: &str) -> Output {
        let mut cmd = self.cli();
        cmd.args(["issue", "checkout", "release", "--repo"])
            .arg(&self.repo)
            .arg("--path")
            .arg(lane)
            .arg("--reason")
            .arg(reason);
        cadence_agent::reaper::output(&mut cmd).expect("run checkout release CLI")
    }

    fn resume(&self, lane: &Path, pinned_sha: &str, reason: &str) -> Output {
        let mut cmd = self.cli();
        assert!(
            !cmd.get_envs()
                .any(|(key, value)| { key == OsStr::new("CADENCE_ALIAS") && value.is_none() }),
            "resume CLI must inherit caller identity"
        );
        cmd.args(["issue", "checkout", "resume", "--repo"])
            .arg(&self.repo)
            .arg("--path")
            .arg(lane)
            .arg("--pinned-sha")
            .arg(pinned_sha)
            .arg("--reason")
            .arg(reason);
        cadence_agent::reaper::output(&mut cmd).expect("run checkout resume CLI")
    }

    fn create_legacy_lane(&self) -> (PathBuf, String) {
        let id = "C84-3";
        let branch = format!("cadence/{id}-legacy");
        let path = self.repo.join(".cadence/wt").join(format!("{id}-legacy"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let path_arg = path.to_str().expect("UTF-8 fixture worktree path");
        git(
            &self.home,
            &self.repo,
            &["worktree", "add", "-b", &branch, path_arg, "main"],
        );
        self.commit_and_merge(&path, &branch, "legacy evidence");
        self.add_issue_refs(id, &path, &branch);
        (path, branch)
    }

    fn add_issue_refs(&self, id: &str, lane: &Path, branch: &str) {
        let file = self.pm.dir.join(PROJECT).join(id).join("issue.md");
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

    fn cli(&self) -> Command {
        let mut cmd = Command::new(BINARY);
        cmd.current_dir(&self.repo)
            .env("CADENCE_PM_DIR", &self.pm.dir)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env_remove("CADENCE_STATE_DIR");
        cmd
    }

    fn retain(&self, lane: &Path, reason: &str) -> Output {
        let mut cmd = self.cli();
        cmd.args(["issue", "checkout", "retain", "--repo"])
            .arg(&self.repo)
            .arg("--path")
            .arg(lane)
            .arg("--reason")
            .arg(reason);
        cadence_agent::reaper::output(&mut cmd).expect("run checkout retain CLI")
    }

    fn finish_command(&self, id: &str, lane: &Path) -> Command {
        let mut cmd = self.cli();
        cmd.args(["issue", "finish", id, "--worktree"])
            .arg(lane)
            .env("CADENCE_TEST_PROC_ROOT", &self.empty_proc_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

struct GitObserver {
    bin: PathBuf,
    real_git: PathBuf,
    entered: PathBuf,
    gate: PathBuf,
    events: PathBuf,
    wait_match: Option<String>,
}

impl GitObserver {
    fn new(fx: &Fixture, name: &str, gate_open: bool) -> Self {
        let dir = fx._root.path().join(name);
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let shim = bin.join("git");
        fs::write(
            &shim,
            "#!/bin/sh\nset -eu\ncase \" $* \" in\n  *\" worktree remove \"*)\n    printf '%s\\n' \"$*\" >> \"$CADENCE_C848_EVENTS\"\n    : > \"$CADENCE_C848_ENTERED\"\n    while [ ! -e \"$CADENCE_C848_GATE\" ]; do sleep 0.01; done\n    ;;\nesac\nif [ -n \"${CADENCE_C848_WAIT_MATCH:-}\" ]; then\n  case \" $* \" in\n    *\"$CADENCE_C848_WAIT_MATCH\"*)\n      : > \"$CADENCE_C848_ENTERED\"\n      while [ ! -e \"$CADENCE_C848_GATE\" ]; do sleep 0.01; done\n      ;;\n  esac\nfi\nexec \"$CADENCE_C848_REAL_GIT\" \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        let observer = Self {
            bin,
            real_git: find_program("git").expect("real Git on PATH"),
            entered: dir.join("entered"),
            gate: dir.join("continue"),
            events: dir.join("events"),
            wait_match: None,
        };
        if gate_open {
            fs::write(&observer.gate, "continue\n").unwrap();
        }
        observer
    }

    fn at_first_matching_git_arg(fx: &Fixture, name: &str, needle: String) -> Self {
        let mut observer = Self::new(fx, name, false);
        observer.wait_match = Some(needle);
        observer
    }

    fn configure(&self, cmd: &mut Command) {
        let path = env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![self.bin.clone()];
        entries.extend(env::split_paths(&path));
        let prefixed = env::join_paths(entries).expect("compose Git shim PATH");
        cmd.env("PATH", prefixed)
            .env("CADENCE_C848_REAL_GIT", &self.real_git)
            .env("CADENCE_C848_ENTERED", &self.entered)
            .env("CADENCE_C848_GATE", &self.gate)
            .env("CADENCE_C848_EVENTS", &self.events)
            .env(
                "CADENCE_C848_WAIT_MATCH",
                self.wait_match.as_deref().unwrap_or(""),
            );
    }
}

struct RaceChildren {
    finish: Option<Child>,
    retain: Option<Child>,
    adopt: Option<Child>,
    gate: PathBuf,
}

impl Drop for RaceChildren {
    fn drop(&mut self) {
        let _ = fs::write(&self.gate, "continue\n");
        if let Some(child) = self.finish.as_mut() {
            let _ = child.wait();
        }
        if let Some(child) = self.retain.as_mut() {
            let _ = child.wait();
        }
        if let Some(child) = self.adopt.as_mut() {
            let _ = child.wait();
        }
    }
}

fn assert_retained(fx: &Fixture, path: &Path, reason: &str) {
    let record = lifecycle::managed_record(&fx.repo, path)
        .expect("read lifecycle record")
        .expect("managed record remains");
    assert_eq!(record.state, "retained");
    assert_eq!(record.retention_reason.as_deref(), Some(reason));
    assert!(path.is_dir(), "retained checkout disappeared");
}

fn assert_refused(output: &Output, expected: &str) {
    let text = output_text(output).to_ascii_lowercase();
    assert!(
        !output.status.success(),
        "operation unexpectedly succeeded: {text}"
    );
    assert!(
        text.contains(expected),
        "refusal omitted {expected:?}: {text}"
    );
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn lifecycle_lock_is_held(repo: &Path) -> io::Result<bool> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(repo.join(".cadence").join("managed-checkouts.lock"))?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(error)
    }
}

fn wait_for_file(path: &Path, child: &mut Child) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("finish exited before Git barrier: {status}"));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for Git barrier {}",
                path.display()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn age_tracked_files(lane: &Path) {
    let then = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(2 * 60 * 60);
    for name in [".gitignore", "tracked.txt", "retained-evidence.txt"] {
        let mut cmd = Command::new("touch");
        cmd.args(["-h", "-d", &format!("@{then}")])
            .arg(lane.join(name));
        let out = cadence_agent::reaper::output(&mut cmd).expect("age fixture tracked file");
        assert!(out.status.success(), "touch failed for {name}");
    }
}

fn find_program(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(find_program("git").expect("Git on PATH"));
    cmd.args([
        OsStr::new("-c"),
        OsStr::new("user.name=CAD-848 retention"),
        OsStr::new("-c"),
        OsStr::new("user.email=retention@invalid"),
    ])
    .args(args)
    .current_dir(cwd)
    .env("HOME", home)
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
    .env_remove("GIT_DIR")
    .env_remove("GIT_WORK_TREE")
    .env_remove("GIT_INDEX_FILE");
    let output = cadence_agent::reaper::output(&mut cmd).expect("run fixture Git");
    assert!(
        output.status.success(),
        "git {} in {} failed: {}{}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
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
