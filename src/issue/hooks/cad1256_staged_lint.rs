//! CAD-1256: the managed pre-commit hook lints the staged issues, not the
//! whole tracker, and still refuses an invalid staged issue. The hook runs
//! `cadence issue lint --staged`; the shim below stands in for the `cadence`
//! binary by re-running this test binary into `shim`, which calls the same
//! `lint::run_staged` the CLI does.
use super::*;
use crate::issue::{lint, write, Pm};
use std::process::{Command, Output};

const N: usize = 300;

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .arg("-C")
        .arg(dir)
        .args(args);
    crate::reaper::output(&mut cmd).unwrap()
}

fn git_ok(dir: &Path, args: &[&str]) {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Runs only when the hook's `cadence` shim re-enters the test binary.
#[test]
fn shim() {
    let Ok(dir) = std::env::var("CAD1256_SHIM_PM") else {
        return;
    };
    let pm = Pm::at(Path::new(&dir)).unwrap();
    let report = lint::run_staged(&pm).unwrap();
    if report["ok"].as_bool() != Some(true) {
        eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
        std::process::exit(1);
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    pm: Pm,
    bin: PathBuf,
}

/// A tracker of `N` committed issues with the managed hooks installed
/// and a `cadence` shim ahead of the host binary on the hook's PATH.
fn fixture() -> Fixture {
    let root = tempfile::Builder::new()
        .prefix("c1256-")
        .tempdir_in("/tmp")
        .unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    // Without a remote the post-commit hook is a no-op; the pre-commit
    // hook is installed after the fixture commit so seeding is not linted.
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
        "seed",
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        Some("ACC-1"),
        Some("body"),
        "t",
    )
    .unwrap();
    let repo2 = root.path().join("repo2");
    std::fs::create_dir_all(&repo2).unwrap();
    write::project_add(
        &pm,
        "oth",
        "OTH",
        &[repo2.display().to_string()],
        &[],
        &[],
        None,
    )
    .unwrap();
    let seed = std::fs::read_to_string(pm.dir.join("acc/ACC-1/issue.md")).unwrap();
    for n in 2..=N {
        let dir = pm.dir.join(format!("acc/ACC-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("issue.md"),
            seed.replace("ACC-1", &format!("ACC-{n}")),
        )
        .unwrap();
    }
    git_ok(&pm.dir, &["add", "-A"]);
    git_ok(&pm.dir, &["commit", "-q", "-m", "seed"]);
    install(&pm.dir).unwrap();

    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = std::env::current_exe().unwrap();
    let shim = bin.join("cadence");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}/argv.log'\nCAD1256_SHIM_PM=\"$CADENCE_PM_DIR\" exec '{}' --exact issue::hooks::cad1256_staged_lint::shim --nocapture --test-threads 1 >/dev/null\n",
            bin.display(),
            exe.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(lint::run_with(&pm, None, None, None).unwrap()["ok"]
        .as_bool()
        .unwrap());
    Fixture {
        _root: root,
        pm,
        bin,
    }
}

impl Fixture {
    fn commit(&self) -> Output {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap());
        let mut cmd = Command::new("git");
        cmd.args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .arg("-C")
            .arg(&self.pm.dir)
            .args(["commit", "-q", "-m", "edit"])
            .env("PATH", path);
        crate::reaper::output(&mut cmd).unwrap()
    }
    fn stage_issue(&self, n: usize, edit: impl Fn(String) -> String) {
        let p = self.pm.dir.join(format!("acc/ACC-{n}/issue.md"));
        let text = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, edit(text)).unwrap();
        git_ok(&self.pm.dir, &["add", &format!("acc/ACC-{n}/issue.md")]);
    }
}

#[test]
fn staged_lint_covers_only_the_staged_issue_in_a_300_issue_tracker() {
    let f = fixture();
    // A path with spaces under the issue folder rides along safely.
    f.stage_issue(7, |t| t.replace("seed", "retitled"));
    let art = f.pm.dir.join("acc/ACC-7/artifacts");
    std::fs::create_dir_all(&art).unwrap();
    std::fs::write(art.join("my notes.txt"), "x").unwrap();
    git_ok(&f.pm.dir, &["add", "acc/ACC-7/artifacts/my notes.txt"]);

    let t0 = std::time::Instant::now();
    let staged = lint::run_staged(&f.pm).unwrap();
    let staged_took = t0.elapsed();
    assert_eq!(staged["ok"], true, "{staged}");
    assert_eq!(staged["scope"], "staged");
    assert_eq!(staged["issues"], 1, "scope must be the one staged issue");
    let t1 = std::time::Instant::now();
    let full = lint::run_with(&f.pm, None, None, None).unwrap();
    eprintln!(
        "staged lint {staged_took:?} vs full lint {:?} ({N} issues)",
        t1.elapsed()
    );
    assert_eq!(full["issues"], N, "the default stays the whole tracker");
    assert_eq!(full["scope"], "all");

    // Through the real hook: commit succeeds and ran `issue lint --staged`.
    let out = f.commit();
    assert!(out.status.success(), "{out:?}");
    let argv = std::fs::read_to_string(f.bin.join("argv.log")).unwrap();
    assert_eq!(argv.trim(), "issue lint --staged");
}

#[test]
fn nothing_staged_is_a_no_op() {
    let f = fixture();
    let r = lint::run_staged(&f.pm).unwrap();
    assert_eq!(
        (r["ok"].clone(), r["issues"].clone()),
        (true.into(), 0.into())
    );
}

#[test]
fn an_invalid_staged_issue_still_refuses_the_commit() {
    let f = fixture();
    let head = git(&f.pm.dir, &["rev-parse", "HEAD"]).stdout;
    f.stage_issue(9, |t| t.replace("status: backlog", "status: bogus"));
    let out = f.commit();
    assert!(!out.status.success(), "commit must be refused: {out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("cadence issue lint failed; commit refused:"),
        "{err}"
    );
    assert!(err.contains("ACC-9: unknown status 'bogus'"), "{err}");
    assert_eq!(git(&f.pm.dir, &["rev-parse", "HEAD"]).stdout, head);
}

fn add_field(t: String, line: &str) -> String {
    t.replacen("\n---\n", &format!("\n{line}\n---\n"), 1)
}

#[test]
fn a_staged_link_to_a_missing_issue_is_refused_and_to_an_existing_one_is_not() {
    let f = fixture();
    f.stage_issue(11, |t| add_field(t, "blocked_by: [ACC-9999]"));
    let r = lint::run_staged(&f.pm).unwrap();
    assert!(
        r["errors"]
            .to_string()
            .contains("dangling link target 'ACC-9999'"),
        "{r}"
    );
    git_ok(&f.pm.dir, &["reset", "-q", "--hard"]);
    f.stage_issue(11, |t| add_field(t, "blocked_by: [ACC-13]"));
    assert_eq!(lint::run_staged(&f.pm).unwrap()["ok"], true);
}

#[test]
fn a_staged_cycle_through_unstaged_issues_is_refused() {
    let f = fixture();
    // ACC-13 -> ACC-14 is already committed; staging ACC-14 -> ACC-13
    // closes the loop, which only resolving the unstaged side can see.
    f.stage_issue(13, |t| add_field(t, "blocked_by: [ACC-14]"));
    git_ok(&f.pm.dir, &["commit", "-q", "--no-verify", "-m", "a"]);
    f.stage_issue(14, |t| add_field(t, "blocked_by: [ACC-13]"));
    let r = lint::run_staged(&f.pm).unwrap();
    assert!(r["errors"].to_string().contains("blocked_by cycle"), "{r}");
}

#[test]
fn a_staged_duplicate_id_in_another_project_is_refused() {
    let f = fixture();
    let dir = f.pm.dir.join("oth/ACC-5");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(f.pm.dir.join("acc/ACC-5/issue.md"), dir.join("issue.md")).unwrap();
    git_ok(&f.pm.dir, &["add", "oth/ACC-5/issue.md"]);
    let r = lint::run_staged(&f.pm).unwrap();
    assert!(r["errors"].to_string().contains("duplicated id"), "{r}");
}
