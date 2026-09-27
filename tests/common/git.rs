use std::path::Path;

/// `git -C <dir> <args>` asserting success, echoing the args and stderr
/// on failure — the verbatim inline closure of the dispatch lanes.
pub fn git_ok() -> impl Fn(&Path, &[&str]) {
    |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

/// [`git_ok`]'s twin returning the trimmed stdout — for lanes that
/// capture `rev-parse` output mid-setup.
pub fn git_stdout() -> impl Fn(&Path, &[&str]) -> String {
    |dir: &Path, args: &[&str]| -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }
}

/// The one-file `f`/`x` repo the dispatch lanes bootstrap: `init -b
/// main`, the `t@t`/`t` identity, `add -A`, `commit -qm init`. Runs
/// through the caller's own `git` closure (assert variants differ per
/// lane). `extra` fires between the write and the add — e.g. the
/// CAD-95 lane plants a Cargo.toml + .gitignore there.
pub fn git_f_repo<R>(repo: &Path, git: &dyn Fn(&Path, &[&str]) -> R, extra: impl FnOnce(&Path)) {
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    extra(repo);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "init"]);
}

/// `git init` + one empty commit so `worktree add -b` has a HEAD.
pub fn git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    }
}

/// `git status --porcelain` — empty means the repo is byte-identical
/// to its index+HEAD (audit N8's launch-purity check).
pub fn git_porcelain(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(out.status.success(), "git status: {:?}", out.stderr);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn git_at(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
