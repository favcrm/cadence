//! CAD-1208 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! Replays the 2026-10-08 incident and its boundaries. A crashed writer
//! left the fence marker, and the only snapshot is one this build does not
//! own (an older binary's per-file `cadence-write.dirty`):
//! (a) a pre-existing foreign untracked file must not be blamed, and the
//!     tracker must keep accepting writes;
//! (b) a modified TRACKED file must still refuse (CAD-852 stays fail-closed);
//! (c) a same-version crash, where this build's own snapshot is present,
//!     must still blame a new untracked leftover.
use super::*;

fn tracker() -> (tempfile::TempDir, Pm) {
    let dir = tempfile::tempdir().unwrap();
    let pm = Pm::init(&dir.path().join("pm")).unwrap();
    (dir, pm)
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = std::process::Command::new("git");
    cmd.args([
        "-c",
        "user.name=CAD-1208 accept",
        "-c",
        "user.email=accept@invalid",
    ])
    .arg("-C")
    .arg(dir)
    .args(args);
    let out = crate::reaper::output(&mut cmd).unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Die holding the lock (the fence marker stays), then leave only an
/// older binary's per-file snapshot behind.
fn crash_leaving_only_an_old_snapshot(pm: &Pm, old_snapshot_lines: &[&str]) {
    let mut lock = pm.lock().unwrap();
    lock.armed = false;
    drop(lock);
    let git_dir = pm.dir.join(".git");
    let _ = std::fs::remove_file(git_dir.join(DIRTY_FILE));
    std::fs::write(
        git_dir.join("cadence-write.dirty"),
        old_snapshot_lines.join("\n") + "\n",
    )
    .unwrap();
}

#[test]
fn cad1208_a_foreign_file_is_not_blamed_under_an_older_snapshot() {
    let (_d, pm) = tracker();
    std::fs::create_dir_all(pm.dir.join("cadence/CAD-584/artifacts")).unwrap();
    let foreign = pm.dir.join("cadence/CAD-584/artifacts/review-r1.md");
    std::fs::write(&foreign, "operator's review\n").unwrap();
    drop(pm.lock().unwrap());
    crash_leaving_only_an_old_snapshot(&pm, &["cadence/CAD-584/artifacts/review-r1.md"]);
    let lock = pm.lock();
    assert!(
        lock.is_ok(),
        "a pre-existing foreign file was blamed under an older snapshot: {}",
        lock.err().map(|e| e.to_string()).unwrap_or_default()
    );
    drop(lock);
    assert_eq!(
        std::fs::read_to_string(&foreign).unwrap(),
        "operator's review\n"
    );
}

#[test]
fn cad1208_a_modified_tracked_file_still_refuses_under_an_older_snapshot() {
    let (_d, pm) = tracker();
    std::fs::write(pm.dir.join("tracked.md"), "committed\n").unwrap();
    git(&pm.dir, &["add", "tracked.md"]);
    git(&pm.dir, &["commit", "-q", "-m", "fixture"]);
    drop(pm.lock().unwrap());
    let mut lock = pm.lock().unwrap();
    std::fs::write(pm.dir.join("tracked.md"), "half-rewritten\n").unwrap();
    lock.armed = false;
    drop(lock);
    let git_dir = pm.dir.join(".git");
    let _ = std::fs::remove_file(git_dir.join(DIRTY_FILE));
    std::fs::write(git_dir.join("cadence-write.dirty"), "\n").unwrap();
    let err = pm
        .lock()
        .expect_err("a crash that modified a tracked file must still refuse");
    assert!(err.to_string().contains("tracked.md"), "{err}");
}

#[test]
fn cad1208_a_same_version_crash_still_blames_a_new_untracked_leftover() {
    let (_d, pm) = tracker();
    drop(pm.lock().unwrap());
    let mut lock = pm.lock().unwrap();
    std::thread::sleep(Duration::from_millis(30));
    std::fs::create_dir_all(pm.dir.join("cadence/CAD-9")).unwrap();
    std::fs::write(pm.dir.join("cadence/CAD-9/issue.md"), "half\n").unwrap();
    lock.armed = false;
    drop(lock);
    assert!(pm.dir.join(".git").join(DIRTY_FILE).exists());
    let err = pm
        .lock()
        .expect_err("a same-version crash leftover must still refuse");
    assert!(err.to_string().contains("cadence/CAD-9"), "{err}");
}
