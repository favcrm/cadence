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
        assert!(start.elapsed() < Duration::from_secs(20), "writer not ready");
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
    assert!(got.is_some(), "the next writer must be admitted after SIGKILL");
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

#[test]
fn a_child_exec_does_not_inherit_the_lock() {
    let (_tmp, pm) = tracker();
    let held = pm.lock().unwrap();
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    // No descriptor of the child may point at a tracker lock file.
    for e in std::fs::read_dir(format!("/proc/{}/fd", child.id())).unwrap() {
        let target = std::fs::read_link(e.unwrap().path()).unwrap_or_default();
        let t = target.to_string_lossy().into_owned();
        assert!(
            !t.contains("write.lock") && !t.contains("write.flock"),
            "child inherited {t}"
        );
    }
    drop(held);
    let got = pm.try_lock().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(got.is_some(), "an inherited fd kept the lock past release");
}

#[test]
fn a_legacy_existence_lock_fails_closed_and_is_left_alone() {
    let (_tmp, pm) = tracker();
    let legacy = pm.dir.join(".write.lock");
    std::fs::write(&legacy, "").unwrap();
    assert!(pm.try_lock().unwrap().is_none());
    let start = Instant::now();
    let e = pm.lock().err().unwrap().to_string();
    assert!(start.elapsed() < Duration::from_secs(20));
    assert!(e.contains("legacy"), "diagnostic must name the legacy lock: {e}");
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
    assert!(pm.try_lock().is_err(), "a symlink must be refused");
    assert_eq!(std::fs::read(&target).unwrap(), b"x");
}

#[test]
fn a_crashed_writers_staged_work_is_refused_not_committed() {
    let (_tmp, pm) = tracker();
    let head = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();
    let (child, _) = spawn_writer(&pm, "hold_dirty", false);
    sigkill(child);
    let e = loop {
        match poll_try_lock(&pm, Duration::from_secs(3)) {
            Err(e) => break e.to_string(),
            Ok(Some(_)) => panic!("a crashed writer's staged index was admitted"),
            Ok(None) => panic!("lock never freed after SIGKILL"),
        }
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
    assert!(start.elapsed() < Duration::from_secs(5), "waited out the lock");
}
