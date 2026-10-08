//! CAD-1255 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! A real tracker write (`write::add_comment`, the path the daemon and every
//! CLI take under the PM lock) on a tracker whose `origin` push is slow must
//! not wait for that push. Production's ~/pm pushes to GitHub over SSH; before
//! the fix every write held the PM lock for the whole push.
use super::*;
use crate::issue::{write, Pm};
use std::time::{Duration, Instant};

const PUSH_SECS: u64 = 3;

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = std::process::Command::new("git");
    cmd.args([
        "-c",
        "user.name=cad1255",
        "-c",
        "user.email=cad1255@invalid",
    ])
    .arg("-C")
    .arg(dir)
    .args(args);
    let out = crate::reaper::output(&mut cmd).unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[test]
fn cad1255_a_tracker_write_does_not_wait_for_a_slow_origin_push() {
    let root = tempfile::Builder::new()
        .prefix("c1255acc-")
        .tempdir_in("/tmp")
        .unwrap();
    let origin = root.path().join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "--bare"]);
    let slow = origin.join("hooks/pre-receive");
    std::fs::write(&slow, format!("#!/bin/sh\nsleep {PUSH_SECS}\n")).unwrap();
    std::fs::set_permissions(&slow, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let pm = Pm::init(&root.path().join("pm")).unwrap();
    install(&pm.dir).unwrap();
    // The managed pre-commit hook shells out to whatever `cadence` is on
    // PATH (the host binary); this check is about the post-commit push.
    let _ = std::fs::remove_file(hooks_dir(&pm.dir).unwrap().join("pre-commit"));
    git(
        &pm.dir,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    write::project_add(
        &pm,
        "acc",
        "ACC",
        &[repo.display().to_string()],
        &[],
        &[],
        None,
    )
    .unwrap();
    write::new_issue(
        &pm,
        &repo,
        Some("acc"),
        "push timing",
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        Some("ACC-1"),
        Some("body"),
        "cad1255",
    )
    .unwrap();

    let began = Instant::now();
    write::add_comment(&pm, "ACC-1", "a comment", None, None, None, "cad1255").unwrap();
    let took = began.elapsed();
    assert!(
        took < Duration::from_millis(PUSH_SECS * 1000 / 2),
        "the tracker write waited for the origin push: {took:?} (push takes {PUSH_SECS}s)"
    );
}
