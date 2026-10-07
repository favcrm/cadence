//! CAD-852 adversarial proofs for the tracker write lock. Each writer
//! that must die abruptly is a real child process (this test binary
//! re-executed on `lock_helper`), killed with SIGKILL — not a guard
//! the test drops.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::write::{new_issue, project_add};
use super::*;

fn tracker() -> (tempfile::TempDir, Pm) {
    let dir = tempfile::tempdir().unwrap();
    let pm = Pm::init(&dir.path().join("pm")).unwrap();
    project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
    (dir, pm)
}

fn mk(pm: &Pm, cwd: &Path, title: &str) -> String {
    new_issue(
        pm,
        cwd,
        Some("cadence"),
        title,
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "t",
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Not a test: the body of the child writer. A no-op unless the parent
/// set the role.
#[test]
fn lock_helper() {
    let Ok(role) = std::env::var("CAD852_ROLE") else {
        return;
    };
    let pm = Pm::at(Path::new(&std::env::var("CAD852_PM").unwrap())).unwrap();
    let _lock = pm.lock().unwrap();
    if role == "hold_untracked" {
        let dir = pm.dir.join("crashed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("issue.md"), "half a write\n").unwrap();
    }
    if role == "hold_dirty" {
        let dir = pm.dir.join("crashed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("issue.md"), "half a write\n").unwrap();
        git(&pm.dir, &["add", "--", "crashed/issue.md"]).unwrap();
    }
    if role == "hold_rename" {
        let dir = pm.dir.join("renamed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("victim.txt"), "rename me\n").unwrap();
        git(&pm.dir, &["add", "--", "renamed/victim.txt"]).unwrap();
        git(
            &pm.dir,
            &[
                "-c",
                "user.name=cadence",
                "-c",
                "user.email=cadence@localhost",
                "commit",
                "-q",
                "-m",
                "rename victim",
                "--",
                "renamed/victim.txt",
            ],
        )
        .unwrap();
        git(&pm.dir, &["mv", "renamed/victim.txt", "renamed/moved.txt"]).unwrap();
    }
    std::fs::write(std::env::var("CAD852_READY").unwrap(), "ready").unwrap();
    std::thread::sleep(Duration::from_secs(600));
}

fn spawn_writer(pm: &Pm, role: &str, setsid: bool) -> (Child, PathBuf) {
    let ready = pm.dir.parent().unwrap().join(format!("ready-{role}"));
    let _ = std::fs::remove_file(&ready);
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "issue::lock_tests::lock_helper", "--nocapture"])
        .env("CAD852_ROLE", role)
        .env("CAD852_PM", &pm.dir)
        .env("CAD852_READY", &ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if setsid {
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let child = cmd.spawn().unwrap();
    let start = Instant::now();
    while !ready.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "writer not ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    (child, ready)
}

fn sigkill(mut child: Child) {
    unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
    child.wait().unwrap();
}

/// `try_lock` until it admits (or errs) — a dead writer's lock must
/// free within a few seconds, never after 15.
fn poll_try_lock(pm: &Pm, within: Duration) -> Result<Option<PmLock>> {
    let start = Instant::now();
    loop {
        match pm.try_lock() {
            Ok(None) if start.elapsed() < within => {
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

#[test]
fn sigkilled_writer_does_not_block_the_next_writer() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold", false);
    assert!(pm.try_lock().unwrap().is_none(), "live writer must block");
    sigkill(child);
    let got = poll_try_lock(&pm, Duration::from_secs(3)).unwrap();
    assert!(
        got.is_some(),
        "the next writer must be admitted after SIGKILL"
    );
    drop(got);
    // And the id allocator still works afterwards.
    assert_eq!(mk(&pm, pm.dir.parent().unwrap(), "after"), "CAD-1");
}

#[test]
fn live_detached_setsid_writer_still_blocks() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold", true);
    for _ in 0..10 {
        assert!(pm.try_lock().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(30));
    }
    sigkill(child);
    assert!(poll_try_lock(&pm, Duration::from_secs(3))
        .unwrap()
        .is_some());
}

#[test]
fn concurrent_writers_serialize_with_unique_ids() {
    let (tmp, pm) = tracker();
    let ids: Vec<String> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..8)
            .map(|i| {
                let dir = pm.dir.clone();
                let cwd = tmp.path();
                s.spawn(move || {
                    let pm = Pm::at(&dir).unwrap();
                    mk(&pm, cwd, &format!("t{i}"))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 8, "duplicate ids: {ids:?}");
}

#[test]
fn try_lock_is_nonblocking_while_held() {
    let (_tmp, pm) = tracker();
    let held = pm.lock().unwrap();
    let start = Instant::now();
    assert!(pm.try_lock().unwrap().is_none());
    assert!(start.elapsed() < Duration::from_millis(500));
    drop(held);
    assert!(pm.try_lock().unwrap().is_some());
}

/// Kills and reaps the child on every exit path, panics included.
struct ReapOnDrop(Child);

impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_child_exec_does_not_inherit_the_lock() {
    let (_tmp, pm) = tracker();
    let held = pm.lock().unwrap();
    let guard = ReapOnDrop(
        Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = guard.0.id();
    // `spawn` returns once the child's exec has swapped its address space,
    // but O_CLOEXEC descriptors are closed partway through exec, later than
    // that point (on Linux 6.x `/proc/<pid>/exe` already shows the new image
    // before `do_close_on_exec` runs in `begin_new_exec`). An immediate scan
    // can therefore see a copy that is about to close. Rescan, bounded to 1s,
    // until no descriptor points at a tracker lock file. A real leak keeps
    // the descriptor for the child's whole lifetime and still fails at the
    // deadline.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let mut leaked = Vec::new();
        let mut last_err = None;
        let mut listed = false;
        match std::fs::read_dir(format!("/proc/{pid}/fd")) {
            Ok(entries) => {
                listed = true;
                for e in entries {
                    match e.and_then(|e| std::fs::read_link(e.path())) {
                        Ok(target) => {
                            let t = target.to_string_lossy().into_owned();
                            if t.contains("write.lock") || t.contains("write.flock") {
                                leaked.push(t);
                            }
                        }
                        Err(err) => last_err = Some(err.to_string()),
                    }
                }
            }
            Err(err) => last_err = Some(err.to_string()),
        }
        if listed && leaked.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "child inherited {leaked:?} or its fds were unreadable (listed: {listed}, last read error: {last_err:?})"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(held);
    let got = pm.try_lock().unwrap();
    drop(guard);
    assert!(got.is_some(), "an inherited fd kept the lock past release");
}

#[test]
fn a_legacy_existence_lock_fails_closed_and_is_left_alone() {
    let (_tmp, pm) = tracker();
    let legacy = pm.dir.join(".write.lock");
    std::fs::write(&legacy, "").unwrap();
    assert!(pm.try_lock().unwrap().is_none());
    let start = Instant::now();
    let e = pm
        .lock_for(Duration::from_millis(300))
        .err()
        .unwrap()
        .to_string();
    assert!(start.elapsed() < Duration::from_secs(20));
    assert!(
        e.contains("legacy"),
        "diagnostic must name the legacy lock: {e}"
    );
    assert!(!e.contains("only if the holder is gone"), "{e}");
    assert!(legacy.exists(), "the legacy lock must never be removed");
    assert_eq!(std::fs::read(&legacy).unwrap(), b"");
}

#[test]
fn a_new_holder_blocks_a_legacy_style_writer() {
    let (_tmp, pm) = tracker();
    let _held = pm.lock().unwrap();
    let e = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(pm.dir.join(".write.lock"))
        .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn a_symlinked_coordination_file_fails_closed() {
    let (tmp, pm) = tracker();
    drop(pm.lock().unwrap());
    let flock = pm.dir.join(".git/cadence-write.flock");
    let target = tmp.path().join("elsewhere");
    std::fs::write(&target, "x").unwrap();
    std::fs::remove_file(&flock).unwrap();
    std::os::unix::fs::symlink(&target, &flock).unwrap();
    let e = pm.try_lock().expect_err("a symlink must be refused");
    assert!(
        e.to_string().contains("cadence-write.flock"),
        "path named: {e}"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"x");
}

#[test]
fn a_crashed_writers_staged_work_is_refused_not_committed() {
    let (_tmp, pm) = tracker();
    let head = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let e = match poll_try_lock(&pm, Duration::from_secs(3)) {
        Err(e) => e.to_string(),
        Ok(Some(_)) => panic!("a crashed writer's staged index was admitted"),
        Ok(None) => panic!("lock never freed after SIGKILL"),
    };
    assert!(e.contains("interrupted"), "{e}");
    assert!(e.contains("crashed/issue.md"), "{e}");
    assert_eq!(git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(), head);
    assert!(git(&pm.dir, &["diff", "--cached", "--name-only"])
        .unwrap()
        .contains("crashed/issue.md"));
    // flush_pending must not replay it either.
    assert!(pm.flush_pending("t").is_err());
    assert_eq!(git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(), head);
    // Once the operator resolves the git state, writes resume.
    git(&pm.dir, &["reset", "-q", "--", "crashed/issue.md"]).unwrap();
    std::fs::remove_dir_all(pm.dir.join("crashed")).unwrap();
    assert!(pm.try_lock().unwrap().is_some());
}

#[test]
fn a_tripped_lease_stops_a_writer_waiting_for_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let mut pm = Pm::init(&dir.path().join("pm")).unwrap();
    let ctl = crate::lease::acquire(
        &state,
        &crate::lease::Hosted {
            lease: Some(format!("file:{}", dir.path().join("l").display())),
            lease_ttl_secs: Some(30),
            lease_renew_secs: Some(5),
            flush_timeout_secs: None,
        },
    )
    .unwrap()
    .unwrap();
    pm.attach_lease(ctl.pm_lease());
    let held = pm.lock().unwrap();
    let start = Instant::now();
    let e = std::thread::scope(|s| {
        let w = s.spawn(|| pm.lock().err().map(|e| e.to_string()));
        std::thread::sleep(Duration::from_millis(300));
        ctl.fence().trip("lease lost while waiting");
        w.join().unwrap()
    });
    drop(held);
    let e = e.expect("a revoked waiter must not be admitted");
    assert!(e.contains("lease"), "{e}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "waited out the lock"
    );
}

#[test]
fn lock_state_tells_held_free_legacy_and_io_unknown_apart() {
    let (_tmp, pm) = tracker();
    assert_eq!(pm.lock_state(), LockState::Free);
    let held = pm.lock().unwrap();
    assert!(matches!(pm.lock_state(), LockState::Held { .. }));
    drop(held);
    assert_eq!(pm.lock_state(), LockState::Free);
    std::fs::write(pm.dir.join(".write.lock"), "").unwrap();
    assert!(matches!(pm.lock_state(), LockState::LegacyUnknown { .. }));
    // Forged "owner" content in a legacy file is still unknown.
    std::fs::write(pm.dir.join(".write.lock"), "pid 1 age 999999\n").unwrap();
    assert!(matches!(pm.lock_state(), LockState::LegacyUnknown { .. }));
    std::fs::remove_file(pm.dir.join(".write.lock")).unwrap();
    std::fs::remove_file(pm.dir.join(".git/cadence-write.flock")).ok();
    std::os::unix::fs::symlink("/nonexistent", pm.dir.join(".git/cadence-write.flock")).unwrap();
    assert!(matches!(pm.lock_state(), LockState::IoUnknown(_)));
}

#[test]
fn a_crash_after_commit_leaves_a_clean_tracker_the_next_writer_reuses() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold", false);
    sigkill(child);
    // The stale marker names no interrupted write: clean git state.
    assert!(pm.dir.join(".write.lock").exists());
    assert!(poll_try_lock(&pm, Duration::from_secs(3))
        .unwrap()
        .is_some());
    assert!(!pm.dir.join(".write.lock").exists(), "released cleanly");
}

#[test]
fn a_waiting_writer_is_admitted_when_the_holder_dies() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold", false);
    let got = std::thread::scope(|s| {
        let w = s.spawn(|| pm.lock_for(Duration::from_secs(10)).is_ok());
        std::thread::sleep(Duration::from_millis(300));
        sigkill(child);
        w.join().unwrap()
    });
    assert!(got);
}

#[test]
fn doctor_reports_the_write_lock_state() {
    let (_tmp, pm) = tracker();
    let r = crate::issue::doctor::run(&pm).unwrap();
    assert_eq!(r["write_lock"]["state"], "free", "{r}");
    std::fs::write(pm.dir.join(".write.lock"), "").unwrap();
    let r = crate::issue::doctor::run(&pm).unwrap();
    assert_eq!(r["write_lock"]["state"], "legacy_unknown", "{r}");
    assert_eq!(r["ok"], false, "{r}");
}

// ---- revision 2 (PR #629 reviews) -------------------------------------

fn mkfifo(path: &Path) {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
}

/// Production always has long-lived foreign untracked files. A crash
/// must not turn them into "interrupted" state.
#[test]
fn a_crash_with_a_preexisting_foreign_file_is_admitted_once_its_own_state_is_resolved() {
    let (_tmp, pm) = tracker();
    std::fs::create_dir_all(pm.dir.join("foreign")).unwrap();
    std::fs::write(pm.dir.join("foreign/blob.txt"), "someone else's\n").unwrap();
    let head = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let e = match poll_try_lock(&pm, Duration::from_secs(3)) {
        Err(e) => e.to_string(),
        Ok(Some(_)) => panic!("a crashed writer's staged index was admitted"),
        Ok(None) => panic!("lock never freed after SIGKILL"),
    };
    assert!(e.contains("crashed/issue.md"), "{e}");
    assert!(
        !e.contains("untracked foreign/blob.txt"),
        "a pre-existing file is not the crashed writer's: {e}"
    );
    // The writer's own leftovers resolved; the foreign file stays.
    git(&pm.dir, &["reset", "-q", "--", "crashed/issue.md"]).unwrap();
    std::fs::remove_dir_all(pm.dir.join("crashed")).unwrap();
    assert!(
        pm.try_lock().unwrap().is_some(),
        "foreign file must not block"
    );
    assert!(pm.dir.join("foreign/blob.txt").exists());
    assert_eq!(git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(), head);
    assert!(!git(&pm.dir, &["ls-files"])
        .unwrap()
        .contains("foreign/blob.txt"));
}

/// The crashed writer's own untracked leftovers stay refused.
#[test]
fn a_crashed_writers_own_untracked_leftovers_are_still_refused() {
    let (_tmp, pm) = tracker();
    std::fs::write(pm.dir.join("foreign.txt"), "x").unwrap();
    let (child, _) = spawn_writer(&pm, "hold_untracked", false);
    sigkill(child);
    let e = match poll_try_lock(&pm, Duration::from_secs(3)) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("own leftovers admitted"),
    };
    assert!(e.contains("crashed/issue.md"), "{e}");
    assert!(!e.contains("untracked foreign.txt"), "{e}");
}

/// Doctor and the write path share one classifier: while every write
/// is refused, doctor is not ok and names the paths.
#[test]
fn doctor_reports_an_interrupted_write_with_the_paths() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let _ = poll_try_lock(&pm, Duration::from_secs(3));
    let r = crate::issue::doctor::run(&pm).unwrap();
    assert_eq!(r["write_lock"]["state"], "interrupted", "{r}");
    assert_eq!(r["write_lock"]["ok"], false, "{r}");
    assert!(
        r["write_lock"]["paths"]
            .to_string()
            .contains("crashed/issue.md"),
        "{r}"
    );
    assert_eq!(r["ok"], false, "{r}");
}

#[test]
fn doctor_stays_ok_after_a_crash_that_left_only_foreign_files() {
    let (_tmp, pm) = tracker();
    std::fs::write(pm.dir.join("foreign.txt"), "x").unwrap();
    let (child, _) = spawn_writer(&pm, "hold", false);
    sigkill(child);
    let r = crate::issue::doctor::run(&pm).unwrap();
    assert_eq!(r["write_lock"]["state"], "free", "{r}");
    assert_eq!(r["write_lock"]["ok"], true, "{r}");
}

#[test]
fn doctor_gives_legacy_unknown_a_next_step_that_is_not_deletion() {
    let (_tmp, pm) = tracker();
    std::fs::write(pm.dir.join(".write.lock"), "").unwrap();
    let r = crate::issue::doctor::run(&pm).unwrap();
    let next = r["write_lock"]["next"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(next.contains("quiescent"), "{r}");
    assert!(next.contains("rollout owner"), "{r}");
}

#[test]
fn a_pm_dir_without_git_is_a_clear_not_a_repository_refusal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pm.yaml"),
        "schema: 1\nnotes_dir: /var/www/agent-notes\n",
    )
    .unwrap();
    let pm = Pm::at(dir.path()).unwrap();
    for e in [
        pm.try_lock().err().unwrap().to_string(),
        pm.lock_for(Duration::from_millis(100))
            .err()
            .unwrap()
            .to_string(),
    ] {
        assert!(e.contains("not a git repository"), "{e}");
    }
}

/// `Pm::init` accepts a gitfile tracker (`--separate-git-dir`, a
/// worktree); it must keep writing.
#[test]
fn a_gitfile_tracker_is_supported() {
    let (tmp, pm) = tracker();
    let store = tmp.path().join("gitstore");
    std::fs::rename(pm.dir.join(".git"), &store).unwrap();
    std::fs::write(
        pm.dir.join(".git"),
        format!("gitdir: {}\n", store.display()),
    )
    .unwrap();
    assert_eq!(mk(&pm, tmp.path(), "via gitfile"), "CAD-1");
    assert!(store.join("cadence-write.flock").is_file());
    assert_eq!(pm.lock_state(), LockState::Free);
}

/// A delayed guard must not unlink a file a successor created.
#[test]
fn a_late_guard_does_not_unlink_a_successors_fence_file() {
    let (_tmp, pm) = tracker();
    let held = pm.lock().unwrap();
    let legacy = pm.dir.join(".write.lock");
    // Keep the old inode alive so the successor's file cannot reuse its
    // number (a freed inode is recycled at once on tmpfs and ext4).
    std::fs::hard_link(&legacy, pm.dir.parent().unwrap().join("old-inode")).unwrap();
    std::fs::remove_file(&legacy).unwrap();
    std::fs::write(&legacy, "successor's lock\n").unwrap();
    drop(held);
    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        b"successor's lock\n",
        "the successor's file must survive the old guard"
    );
}

#[test]
fn a_fifo_or_directory_at_the_coordination_path_fails_closed() {
    for kind in ["fifo", "dir"] {
        let (_tmp, pm) = tracker();
        drop(pm.lock().unwrap());
        let flock = pm.dir.join(".git/cadence-write.flock");
        std::fs::remove_file(&flock).unwrap();
        if kind == "fifo" {
            mkfifo(&flock);
        } else {
            std::fs::create_dir(&flock).unwrap();
        }
        let start = Instant::now();
        let e = pm
            .try_lock()
            .err()
            .unwrap_or_else(|| panic!("{kind} admitted"));
        assert!(e.to_string().contains("cadence-write.flock"), "{kind}: {e}");
        assert!(matches!(pm.lock_state(), LockState::IoUnknown(_)), "{kind}");
        assert!(start.elapsed() < Duration::from_secs(5), "{kind} hung");
        assert!(pm.dir.join(".write.lock").symlink_metadata().is_err());
    }
}

/// A probe racing a non-waiting `try_lock` must not make it see
/// "busy" when nobody holds the lock.
#[test]
fn a_state_probe_does_not_make_try_lock_spuriously_busy() {
    let (_tmp, pm) = tracker();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let spurious = std::thread::scope(|s| {
        s.spawn(|| {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = pm.lock_state();
                std::thread::sleep(Duration::from_micros(300));
            }
        });
        let mut n = 0;
        for _ in 0..1500 {
            match pm.try_lock().unwrap() {
                Some(l) => drop(l),
                None => n += 1,
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        n
    });
    assert_eq!(spurious, 0, "try_lock saw a lock nobody held");
}

/// The temp marker is created exclusively without following links,
/// and stale ones from a crashed writer are swept under the lock.
#[test]
fn the_temp_marker_never_follows_a_symlink_and_stale_ones_are_swept() {
    let (tmp, pm) = tracker();
    let target = tmp.path().join("victim");
    std::fs::write(&target, "precious").unwrap();
    let git_dir = pm.dir.join(".git");
    std::os::unix::fs::symlink(
        &target,
        git_dir.join(format!("cadence-write.tmp-{}", std::process::id())),
    )
    .unwrap();
    std::fs::write(git_dir.join("cadence-write.tmp-999999"), "leaked").unwrap();
    let got = pm.try_lock().unwrap();
    assert!(got.is_some());
    assert_eq!(std::fs::read(&target).unwrap(), b"precious");
    drop(got);
    let left: Vec<_> = std::fs::read_dir(&git_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("cadence-write.tmp-")
        })
        .collect();
    assert!(left.is_empty(), "stale tmp markers survived: {left:?}");
}

/// `.write.lock` as a FIFO or a symlink is a legacy lock of unknown
/// owner; classifying it neither blocks nor follows.
#[test]
fn a_fifo_or_symlink_at_the_fence_path_is_legacy_unknown_and_never_blocks() {
    for kind in ["fifo", "symlink"] {
        let (tmp, pm) = tracker();
        let legacy = pm.dir.join(".write.lock");
        if kind == "fifo" {
            mkfifo(&legacy);
        } else {
            std::fs::write(tmp.path().join("t"), crate::issue::pmlock::MARKER).unwrap();
            std::os::unix::fs::symlink(tmp.path().join("t"), &legacy).unwrap();
        }
        let start = Instant::now();
        assert!(
            matches!(pm.lock_state(), LockState::LegacyUnknown { .. }),
            "{kind}"
        );
        assert!(pm.try_lock().unwrap().is_none(), "{kind}");
        assert!(start.elapsed() < Duration::from_secs(5), "{kind} hung");
        assert!(legacy.symlink_metadata().is_ok(), "{kind} was removed");
    }
}

/// CAD-876: exit 75 means "retry", so only a lock that frees by itself
/// may be `busy`. A legacy `.write.lock` is never removed or aged out by
/// this build: it must be a non-retryable, coded gate.
#[test]
fn a_legacy_lock_refusal_is_a_gate_and_a_live_writer_is_busy() {
    let (_tmp, pm) = tracker();
    let held = pm.lock().unwrap();
    let busy = pm.acquire_for(Duration::from_millis(100)).err().unwrap();
    assert_eq!(busy.kind(), "busy", "{busy}");
    assert_eq!(busy.code(), Some("resource_busy"));
    drop(held);
    std::fs::write(pm.dir.join(".write.lock"), "").unwrap();
    let legacy = pm.acquire_for(Duration::from_millis(100)).err().unwrap();
    assert_eq!(legacy.kind(), "gate", "{legacy}");
    assert_eq!(legacy.code(), Some("legacy_write_lock"));
    assert_eq!(
        crate::error::exit_code_for_kind(legacy.kind()),
        4,
        "legacy lock must not look retryable"
    );
    assert!(legacy.to_string().contains(".write.lock"), "{legacy}");
    assert!(legacy.to_string().contains("rollout owner"), "{legacy}");
}

/// Closes both ends of the `go` pipe on drop, first writing `go`, so a
/// failed assertion never leaves a child parked in `pre_exec`.
struct Release {
    go_w: i32,
    fds: [i32; 3],
}

impl Drop for Release {
    fn drop(&mut self) {
        unsafe {
            let b = 1u8;
            libc::write(self.go_w, (&b as *const u8).cast(), 1);
            libc::close(self.go_w);
            for fd in self.fds {
                libc::close(fd);
            }
        }
    }
}

/// CAD-948: a forked child that has not exec'd yet holds a copy of the
/// lock descriptor. Dropping the guard must still release the lock for
/// everyone (explicit `LOCK_UN`), not only once the child execs.
#[test]
fn a_forked_child_does_not_keep_the_lock_after_the_guard_drops() {
    let (_tmp, pm) = tracker();
    let (mut ready, mut go) = ([0i32; 2], [0i32; 2]);
    unsafe {
        assert_eq!(libc::pipe(ready.as_mut_ptr()), 0);
        assert_eq!(libc::pipe(go.as_mut_ptr()), 0);
    }
    let (ready_w, go_r) = (ready[1], go[0]);
    // Declared before anything can panic: it releases the child.
    let _release = Release {
        go_w: go[1],
        fds: [ready[0], ready[1], go[0]],
    };
    let held = pm.lock().unwrap();
    let spawner = std::thread::spawn(move || {
        let mut cmd = Command::new("true");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            // Async-signal-safe only: tell the parent we forked, then
            // park before exec until told to go.
            cmd.pre_exec(move || {
                let mut b = 1u8;
                libc::write(ready_w, (&b as *const u8).cast(), 1);
                libc::read(go_r, (&mut b as *mut u8).cast(), 1);
                Ok(())
            });
        }
        cmd.status().unwrap()
    });
    let mut b = 0u8;
    assert_eq!(
        unsafe { libc::read(ready[0], (&mut b as *mut u8).cast(), 1) },
        1
    );
    drop(held);
    // The child is parked in pre_exec with a copy of the descriptor.
    assert_eq!(
        pm.lock_state(),
        LockState::Free,
        "a forked copy must not keep the lock after the guard dropped"
    );
    assert!(pm.try_lock().unwrap().is_some());
    unsafe { libc::write(go[1], (&b as *const u8).cast(), 1) };
    assert!(spawner.join().unwrap().success());
    assert_eq!(pm.lock_state(), LockState::Free);
}

// ---- CAD-1167: holder diagnostics + path-scoped admission -----------

/// A live holder's refusal names it: the holder pid appears whether it
/// comes from the /proc scan or the marker trailer.
#[test]
fn a_live_holders_refusal_names_its_pid() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold", false);
    let pid = child.id().to_string();
    let e = pm
        .lock_for(Duration::from_millis(200))
        .err()
        .unwrap()
        .to_string();
    assert!(
        e.contains(&pid),
        "a live holder's refusal names its pid: {e}"
    );
    // The `holder: pid N (comm)` shape is only produced by the live
    // /proc scan: a trailer-only report would say "no live holder
    // found" instead, so this proves the scan ran, not just the file.
    // Off Linux the scan deliberately reports unavailable while the
    // trailer still names the holder pid.
    #[cfg(target_os = "linux")]
    assert!(e.contains("holder: pid"), "no scan attribution: {e}");
    #[cfg(not(target_os = "linux"))]
    {
        assert!(e.contains("holder scan unavailable"), "{e}");
        assert!(e.contains(&format!("pid={pid}")), "{e}");
    }
    #[cfg(target_os = "linux")]
    {
        let comm = std::fs::read_to_string("/proc/self/comm")
            .unwrap()
            .trim()
            .to_string();
        assert!(
            e.contains(&comm),
            "a live holder's refusal names its command ({comm}): {e}"
        );
    }
    sigkill(child);
}

/// Our own fence file carries a diagnostic trailer while held: the
/// exact MARKER prefix plus well-formed key=value lines, ours included.
#[test]
fn a_held_fence_file_carries_a_parseable_trailer() {
    let (_tmp, pm) = tracker();
    let _held = pm.lock().unwrap();
    let content = std::fs::read(pm.dir.join(".write.lock")).unwrap();
    let marker = crate::issue::pmlock::MARKER;
    assert!(
        content.starts_with(marker.as_bytes()),
        "fence file lost its marker"
    );
    let tail = std::str::from_utf8(&content[marker.len()..]).unwrap();
    assert!(
        tail.contains(&format!("pid={}\n", std::process::id())),
        "trailer names no holder pid: {tail}"
    );
    assert!(
        tail.contains("cmd="),
        "trailer names no holder command: {tail}"
    );
    assert!(
        tail.contains(&format!("version={}\n", env!("CARGO_PKG_VERSION"))),
        "trailer names no build version: {tail}"
    );
}

/// A legacy fence left beside a killed holder refuses as the
/// non-retryable gate and says positively that nobody holds it — the
/// CAD-999 production shape: a legacy file appears, its writer is
/// dead, and the next write must diagnose, not wait.
#[test]
fn a_killed_holders_legacy_lock_states_no_live_holder() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold", false);
    std::fs::write(pm.dir.join(".write.lock"), "").unwrap();
    sigkill(child);
    let e = pm.lock_for(Duration::from_millis(100)).err().unwrap();
    assert_eq!(e.code(), Some("legacy_write_lock"), "{e}");
    let text = e.to_string();
    #[cfg(target_os = "linux")]
    assert!(text.contains("no live holder found"), "{text}");
    #[cfg(not(target_os = "linux"))]
    assert!(text.contains("holder scan unavailable"), "{text}");
    std::fs::remove_file(pm.dir.join(".write.lock")).unwrap();
}

/// MARKER plus a malformed tail is a legacy lock of unknown owner:
/// never reused, never removed. A well-formed trailer still admits.
#[test]
fn a_malformed_marker_tail_is_legacy_unknown() {
    let marker = crate::issue::pmlock::MARKER;
    for tail in [
        "bogus\n",
        "pid=1\npid=2\n",
        "pid=abc\n",
        "pid=\n",
        "unexpected=1\n",
        "==",
    ] {
        let (_tmp, pm) = tracker();
        std::fs::write(pm.dir.join(".write.lock"), format!("{marker}{tail}")).unwrap();
        assert!(
            matches!(pm.lock_state(), LockState::LegacyUnknown { .. }),
            "malformed tail admitted: {tail:?}"
        );
        assert!(
            pm.try_lock().unwrap().is_none(),
            "malformed tail admitted: {tail:?}"
        );
        assert!(
            pm.dir.join(".write.lock").exists(),
            "malformed tail removed: {tail:?}"
        );
    }
    let (_tmp, pm) = tracker();
    std::fs::write(
        pm.dir.join(".write.lock"),
        format!("{marker}pid=1\nhost=h\nversion=9.9\nstarted_at=1\ncmd=cadence\n"),
    )
    .unwrap();
    assert!(
        pm.try_lock().unwrap().is_some(),
        "a well-formed trailer must not break reuse"
    );
}

/// Path-scoped admission: after a crash that staged `crashed/issue.md`,
/// a write scoped to an unrelated issue is admitted and commits, while
/// an overlapping scoped write — and every unscoped one — is refused.
#[test]
fn a_disjoint_write_is_admitted_beside_an_interrupted_one() {
    let (_tmp, pm) = tracker();
    mk(&pm, pm.dir.parent().unwrap(), "first");
    let other = pm.dir.join("cadence").join("CAD-1");
    let head = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    // Unscoped: refused, as before.
    let e = pm.try_lock().err().unwrap().to_string();
    assert!(e.contains("interrupted"), "{e}");
    // Scoped to the unrelated issue: admitted, and the commit lands.
    let lock = pm
        .lock_for_paths(std::slice::from_ref(&other))
        .expect("a disjoint write must be admitted");
    let file = other.join("issue.md");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\nscoped write\n");
    std::fs::write(&file, text).unwrap();
    pm.commit_scoped(&lock, std::slice::from_ref(&file), "scoped test commit")
        .unwrap();
    // A commit through the same scoped lock that touches the crash's
    // paths is refused before any staging: HEAD, index bytes and the
    // crashed file itself are byte-identical afterwards.
    let head_before = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();
    let index_before = git(&pm.dir, &["diff", "--cached"]).unwrap();
    let crashed = pm.dir.join("crashed").join("issue.md");
    let file_before = std::fs::read(&crashed).unwrap();
    let e = pm
        .commit_scoped(&lock, std::slice::from_ref(&crashed), "overlap the crash")
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("overlaps"), "{e}");
    assert!(e.contains("crashed/issue.md"), "{e}");
    assert_eq!(
        git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(),
        head_before,
        "a refused commit moved HEAD"
    );
    assert_eq!(
        git(&pm.dir, &["diff", "--cached"]).unwrap(),
        index_before,
        "a refused commit touched the index"
    );
    assert_eq!(
        std::fs::read(&crashed).unwrap(),
        file_before,
        "a refused commit touched the crashed file"
    );
    drop(lock);
    assert_ne!(git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(), head);
    // The scoped admission must not consume the tripwire: the stale
    // marker stays, and an unscoped write is still refused.
    assert!(pm.dir.join(".write.lock").exists());
    let e = pm.try_lock().err().unwrap().to_string();
    assert!(e.contains("interrupted"), "tripwire consumed: {e}");
    // The crash's own leftovers are untouched by that commit.
    assert!(git(&pm.dir, &["diff", "--cached", "--name-only"])
        .unwrap()
        .contains("crashed/issue.md"));
    // Scoped to the crash: refused, naming it.
    let e = pm
        .lock_for_paths(std::slice::from_ref(&pm.dir.join("crashed")))
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("crashed/issue.md"), "{e}");
}

/// A commit outside the admitted set refuses even on a clean tracker:
/// scoped admission is enforced at commit time, not just at acquire —
/// and the refusal stages nothing.
#[test]
fn a_scoped_lock_refuses_a_commit_outside_its_paths() {
    let (_tmp, pm) = tracker();
    mk(&pm, pm.dir.parent().unwrap(), "first");
    let a = pm.dir.join("cadence").join("CAD-1");
    let lock = pm.lock_for_paths(std::slice::from_ref(&a)).unwrap();
    let elsewhere = pm.dir.join("elsewhere.md");
    std::fs::write(&elsewhere, "x\n").unwrap();
    let e = pm
        .commit_scoped(&lock, std::slice::from_ref(&elsewhere), "out of scope")
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("outside"), "{e}");
    drop(lock);
    assert!(
        git(&pm.dir, &["status", "--porcelain"])
            .unwrap()
            .contains("elsewhere.md"),
        "the refused commit must leave the file uncommitted"
    );
}

/// Git-level markers still refuse every write, disjoint or not — and
/// clearing the marker restores disjoint admission.
#[test]
fn a_merge_marker_refuses_even_disjoint_writes() {
    let (_tmp, pm) = tracker();
    mk(&pm, pm.dir.parent().unwrap(), "first");
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    std::fs::write(pm.dir.join(".git").join("MERGE_HEAD"), "deadbeef\n").unwrap();
    let other = pm.dir.join("cadence").join("CAD-1");
    let e = pm
        .lock_for_paths(std::slice::from_ref(&other))
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("MERGE_HEAD"), "{e}");
    std::fs::remove_file(pm.dir.join(".git").join("MERGE_HEAD")).unwrap();
    // A stale index lock is the same class of global marker: it
    // refuses disjoint writes until git itself clears it.
    std::fs::write(pm.dir.join(".git").join("index.lock"), "").unwrap();
    let e = pm
        .lock_for_paths(std::slice::from_ref(&other))
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("index.lock"), "{e}");
    std::fs::remove_file(pm.dir.join(".git").join("index.lock")).unwrap();
    assert!(
        pm.lock_for_paths(std::slice::from_ref(&other)).is_ok(),
        "disjoint writes resume once the global marker clears"
    );
}

/// A tripped lease stops a scoped waiter too: the fence points are
/// shared with the unscoped path, not reimplemented beside it.
#[test]
fn a_tripped_lease_stops_a_scoped_writer_waiting_for_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let mut pm = Pm::init(&dir.path().join("pm")).unwrap();
    let ctl = crate::lease::acquire(
        &state,
        &crate::lease::Hosted {
            lease: Some(format!("file:{}", dir.path().join("l").display())),
            lease_ttl_secs: Some(30),
            lease_renew_secs: Some(5),
            flush_timeout_secs: None,
        },
    )
    .unwrap()
    .unwrap();
    pm.attach_lease(ctl.pm_lease());
    let held = pm.lock().unwrap();
    let start = Instant::now();
    let scope = pm.dir.clone();
    let e = std::thread::scope(|s| {
        let w = s.spawn(|| {
            pm.lock_for_paths(std::slice::from_ref(&scope))
                .err()
                .map(|e| e.to_string())
        });
        std::thread::sleep(Duration::from_millis(300));
        ctl.fence().trip("lease lost while waiting");
        w.join().unwrap()
    });
    drop(held);
    let e = e.expect("a revoked scoped waiter must not be admitted");
    assert!(e.contains("lease"), "{e}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "waited out the lock"
    );
}

/// A `..` component is refused as a caller bug in both admission and
/// commit: git would resolve `dir/../../x` to `x`, letting a scoped
/// write pass the checks as one path and commit another.
#[test]
fn dotdot_paths_are_refused_as_caller_bugs() {
    let (_tmp, pm) = tracker();
    mk(&pm, pm.dir.parent().unwrap(), "first");
    let evil: PathBuf = pm.dir.join("cadence").join("..").join("elsewhere.md");
    let e = pm
        .lock_for_paths(std::slice::from_ref(&evil))
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("escapes"), "{e}");
    let lock = pm.lock().unwrap();
    let e = pm
        .commit_scoped(&lock, std::slice::from_ref(&evil), "escape")
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("escapes"), "{e}");
}

/// The tracker root is the whole tracker: it admits and commits
/// ordinary files on a clean tree, and overlaps every leftover
/// beside a crash — never silently disjoint.
#[test]
fn tracker_root_scope_covers_the_whole_tracker() {
    let (_tmp, pm) = tracker();
    mk(&pm, pm.dir.parent().unwrap(), "first");
    let file = pm.dir.join("cadence").join("CAD-1").join("issue.md");
    let lock = pm.lock_for_paths(std::slice::from_ref(&pm.dir)).unwrap();
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\nroot scoped\n");
    std::fs::write(&file, text).unwrap();
    pm.commit_scoped(&lock, std::slice::from_ref(&file), "root scoped commit")
        .unwrap();
    drop(lock);
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let e = pm
        .lock_for_paths(std::slice::from_ref(&pm.dir))
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("interrupted"), "root wrongly disjoint: {e}");
}

/// Guard against API misuse: every write.rs function that takes a
/// scoped lock must commit only through the scoped wrappers. Plain
/// commit never checks scope, so an unscoped call beside
/// `lock_for_paths` would silently write outside the admitted set.
/// This test reads the source it guards and fails the build on drift.
#[test]
fn write_rs_pairs_scoped_locks_with_scoped_commits() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/issue/write.rs"),
    )
    .unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let mut bounds = vec![0usize];
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("pub fn ") || l.starts_with("pub(crate) fn ") || l.starts_with("fn ") {
            bounds.push(i);
        }
    }
    bounds.push(lines.len());
    let mut bad = Vec::new();
    for w in bounds.windows(2) {
        let body = lines[w[0]..w[1]].join("\n");
        if !body.contains("lock_for_paths") {
            continue;
        }
        for pat in ["commit(", "commit_who(", "commit_staged("] {
            let mut search = body.as_str();
            while let Some(pos) = search.find(pat) {
                // `fn commit...(` definitions are the wrappers themselves.
                if search[..pos].ends_with("fn ") {
                    search = &search[pos + pat.len()..];
                    continue;
                }
                bad.push(format!(
                    "{}: unscoped {pat} beside lock_for_paths",
                    lines[w[0]]
                ));
                break;
            }
        }
    }
    assert!(
        bad.is_empty(),
        "scoped locks must pair with scoped commits:\n{}",
        bad.join("\n")
    );
}

/// A staged rename blocks BOTH ends: the destination is classified,
/// and so is the source the crashed writer deleted. A scoped write to
/// either is refused; an unrelated path is still admitted.
#[test]
fn a_staged_rename_blocks_both_ends() {
    let (_tmp, pm) = tracker();
    let (child, _) = spawn_writer(&pm, "hold_rename", false);
    sigkill(child);
    for end in ["renamed/victim.txt", "renamed/moved.txt"] {
        let e = pm
            .lock_for_paths(std::slice::from_ref(&pm.dir.join(end)))
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains(end), "rename end not blocked ({end}): {e}");
    }
    mk(&pm, pm.dir.parent().unwrap(), "first");
    let other = pm.dir.join("cadence").join("CAD-1");
    assert!(
        pm.lock_for_paths(std::slice::from_ref(&other)).is_ok(),
        "an unrelated path must stay admitted beside a staged rename"
    );
}

/// Git pathspec syntax is refused as a caller bug in both admission
/// and commit: git would expand the pattern beyond the literal string
/// the scope checked.
#[test]
fn pathspec_syntax_is_refused_as_caller_bug() {
    let (_tmp, pm) = tracker();
    mk(&pm, pm.dir.parent().unwrap(), "first");
    for evil in [
        "cadence/*.md",
        "cadence/CAD-?/issue.md",
        "cadence/[C]AD-1/issue.md",
        ":(top)cadence",
        ":!cadence/CAD-1/issue.md",
        "back\\slash.md",
    ] {
        let p = pm.dir.join(evil);
        let e = pm
            .lock_for_paths(std::slice::from_ref(&p))
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("pathspec"), "{evil}: {e}");
    }
    let lock = pm.lock().unwrap();
    let e = pm
        .commit_scoped(
            &lock,
            std::slice::from_ref(&pm.dir.join("cadence/*.md")),
            "expand",
        )
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("pathspec"), "{e}");
}
