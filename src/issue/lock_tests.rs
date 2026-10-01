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
    assert_eq!(pm.lock_state(), LockState::Held);
    drop(held);
    assert_eq!(pm.lock_state(), LockState::Free);
    std::fs::write(pm.dir.join(".write.lock"), "").unwrap();
    assert_eq!(pm.lock_state(), LockState::LegacyUnknown);
    // Forged "owner" content in a legacy file is still unknown.
    std::fs::write(pm.dir.join(".write.lock"), "pid 1 age 999999\n").unwrap();
    assert_eq!(pm.lock_state(), LockState::LegacyUnknown);
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
        assert_eq!(pm.lock_state(), LockState::LegacyUnknown, "{kind}");
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
