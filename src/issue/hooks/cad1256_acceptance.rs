//! CAD-1256 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! The managed pre-commit lint is a gate. Narrowing it to `--staged` must
//! not freeze a tracker whose PATH `cadence` predates the flag (the
//! newer-artefact / older-binary class of 2026-10-08, CAD-1208), and it
//! must never stop refusing an invalid commit. Exercised on the exact
//! PRE_COMMIT text with stand-in `cadence` binaries in a plain git repo.
use super::*;
use std::os::unix::fs::PermissionsExt;

/// A stand-in `cadence`. `new` = it knows `--staged`. Its "lint" fails when
/// any staged file contains BOGUS. Every call is logged.
fn shim(dir: &Path, log: &Path, new: bool) {
    let staged_help = if new {
        "      --staged   Lint only staged issues"
    } else {
        ""
    };
    let staged_run = if new {
        "if git diff --cached --name-only | xargs -r grep -l BOGUS >/dev/null 2>&1; then echo 'lint: bogus' >&2; exit 1; fi; exit 0"
    } else {
        "echo \"error: unexpected argument '--staged' found\" >&2; exit 2"
    };
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\n\
         if [ \"$3\" = \"--help\" ]; then printf 'Usage: cadence issue lint [OPTIONS]\\n{staged_help}\\n'; exit 0; fi\n\
         if [ \"$3\" = \"--staged\" ]; then {staged_run}; fi\n\
         if git diff --cached --name-only | xargs -r grep -l BOGUS >/dev/null 2>&1; then echo 'lint: bogus' >&2; exit 1; fi\n\
         exit 0\n",
        log = log.display()
    );
    let p = dir.join("cadence");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(repo: &Path, path: &str, args: &[&str]) -> std::process::Output {
    let mut cmd = std::process::Command::new("git");
    cmd.args([
        "-c",
        "user.name=cad1256",
        "-c",
        "user.email=cad1256@invalid",
    ])
    .arg("-C")
    .arg(repo)
    .args(args)
    .env("PATH", path);
    crate::reaper::output(&mut cmd).unwrap()
}

fn head(repo: &Path, path: &str) -> String {
    String::from_utf8_lossy(&git(repo, path, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn rig(
    new: bool,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
) {
    let root = tempfile::Builder::new()
        .prefix("c1256acc-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = root.path().join("pm");
    let bin = root.path().join("bin");
    let log = root.path().join("calls.log");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    shim(&bin, &log, new);
    let path = format!("{}:/usr/bin:/bin", bin.display());
    assert!(git(&repo, &path, &["init", "-q", "-b", "main"])
        .status
        .success());
    std::fs::write(repo.join("seed.md"), "seed\n").unwrap();
    git(&repo, &path, &["add", "seed.md"]);
    assert!(git(&repo, &path, &["commit", "-q", "-m", "seed"])
        .status
        .success());
    let hook = repo.join(".git/hooks/pre-commit");
    std::fs::write(&hook, PRE_COMMIT).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    (root, repo, log, path)
}

#[test]
fn cad1256_an_older_cadence_never_freezes_commits_and_still_refuses_bad_ones() {
    let (_root, repo, log, path) = rig(false);
    std::fs::write(repo.join("ok.md"), "fine\n").unwrap();
    git(&repo, &path, &["add", "ok.md"]);
    let ok = git(&repo, &path, &["commit", "-q", "-m", "valid"]);
    assert!(
        ok.status.success(),
        "an older cadence froze a valid commit: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let before = head(&repo, &path);
    std::fs::write(repo.join("bad.md"), "status: BOGUS\n").unwrap();
    git(&repo, &path, &["add", "bad.md"]);
    let bad = git(&repo, &path, &["commit", "-q", "-m", "invalid"]);
    assert!(!bad.status.success(), "an invalid commit was accepted");
    assert!(String::from_utf8_lossy(&bad.stderr).contains("commit refused"));
    assert_eq!(head(&repo, &path), before, "a refused commit moved HEAD");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.lines().any(|l| l.trim() == "issue lint --staged"),
        "{calls}"
    );
}

#[test]
fn cad1256_a_newer_cadence_lints_only_staged_and_still_refuses_bad_ones() {
    let (_root, repo, log, path) = rig(true);
    std::fs::write(repo.join("ok.md"), "fine\n").unwrap();
    git(&repo, &path, &["add", "ok.md"]);
    assert!(git(&repo, &path, &["commit", "-q", "-m", "valid"])
        .status
        .success());
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.lines().any(|l| l.trim() == "issue lint --staged"),
        "{calls}"
    );
    std::fs::write(repo.join("bad.md"), "status: BOGUS\n").unwrap();
    git(&repo, &path, &["add", "bad.md"]);
    let bad = git(&repo, &path, &["commit", "-q", "-m", "invalid"]);
    assert!(
        !bad.status.success(),
        "an invalid commit was accepted by --staged"
    );
}
