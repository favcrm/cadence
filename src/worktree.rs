//! `.cadence/wt/<name>` worktree helpers — `cadence devin --worktree`
//! mints agent checkouts and `cadence issue start` mints issue-bound
//! ones; both share the same layout (`<root>/.cadence/wt/<name>` on
//! `cadence/<name>`) and the `.cadence/` ignore rule.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::issue::{git, project};
use crate::proto;

/// Keep `.cadence/` out of a repo's index: append the entry to its
/// `.gitignore` when nothing already covers it.
pub fn ensure_cadence_ignored(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let covered = existing.lines().any(|l| {
        matches!(
            l.trim(),
            ".cadence" | ".cadence/" | "/.cadence" | "/.cadence/"
        )
    });
    if !covered {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, ".cadence/")?;
    }
    Ok(())
}

/// The main checkout root for `dir` — `--git-common-dir` maps linked
/// worktrees back to the same root, so `…/.cadence/wt/` always lands
/// under the primary checkout rather than nested inside a worktree.
pub fn main_root(dir: &Path) -> Result<PathBuf> {
    let (root, _) = project::repo_identity(dir).ok_or_else(|| {
        Error::rejected(format!(
            "'{}' is not inside a git repository",
            dir.display()
        ))
    })?;
    Ok(root)
}

/// `git worktree add <dir> [-b <branch>] <base>` — the bare plumbing;
/// existence and idempotency decisions are the caller's.
pub fn add(root: &Path, dir: &Path, branch: Option<&str>, base: &str) -> Result<()> {
    let target = dir.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "add", &target];
    if let Some(branch) = branch {
        args.push("-b");
        args.push(branch);
    }
    args.push(base);
    git(root, &args)?;
    Ok(())
}

/// `git worktree add <root>/.cadence/wt/<name> -b cadence/<name>` — the
/// new checkout becomes the agent's cwd. Clean failures: no git repo
/// under `base`, a pre-existing worktree dir, or a branch collision.
pub fn create_worktree(base: &Path, name: &str) -> Result<PathBuf> {
    proto::identifier(name, "Worktree name")?;
    let root = match git(base, &["rev-parse", "--show-toplevel"]) {
        Ok(root) => PathBuf::from(root),
        Err(_) => {
            return Err(Error::rejected(format!(
                "--worktree requires a git repository — '{}' is not inside one",
                base.display()
            )))
        }
    };
    let dir = root.join(".cadence").join("wt").join(name);
    if dir.exists() {
        return Err(Error::rejected(format!(
            "Worktree '{name}' already exists at {} — reuse it with \
             --cwd {}",
            dir.display(),
            dir.display()
        )));
    }
    let branch = format!("cadence/{name}");
    add(&root, &dir, Some(&branch), "HEAD").map_err(|e| {
        Error::rejected(format!(
            "{e} — if branch '{branch}' already exists, reuse the checkout \
             with --cwd or pick another --worktree name"
        ))
    })?;
    ensure_cadence_ignored(&root)?;
    Ok(dir)
}

/// The shared cargo target dir for a repo — `<root>/.cadence/target/
/// shared`. One cache for every `.cadence/wt` lane: dependency
/// artifacts are built once per host instead of once per worktree.
pub fn shared_target_dir(root: &Path) -> PathBuf {
    root.join(".cadence").join("target").join("shared")
}

/// The effective cargo target dir for a worktree: the shared cache
/// unless the project's `[build] target_dir = "per-worktree"` opts it
/// back onto the classic per-lane `target/`.
pub fn target_dir_for(project: &project::Project, root: &Path, wt_dir: &Path) -> Result<PathBuf> {
    match project.build.as_ref().and_then(|b| b.target_dir.as_deref()) {
        None | Some("shared") => Ok(shared_target_dir(root)),
        Some("per-worktree") => Ok(wt_dir.join("target")),
        Some(other) => Err(Error::rejected(format!(
            "[build] target_dir = \"{other}\" in {}'s project.yaml — \
             expected \"shared\" or \"per-worktree\"",
            project.key
        ))),
    }
}

/// Point a worktree's cargo builds at `target`: merge
/// `build.target-dir` into `<wt>/.cargo/config.toml` and mark `.cargo/`
/// ignored in that worktree's own `info/exclude` — never the repo's
/// `.gitignore` — so the dirty checks in `issue finish` and
/// `doctor --host` still see a clean tree. An explicit `target-dir`
/// the operator already wrote wins over ours; the return is the
/// *effective* dir (theirs or ours) for the worktree ref.
pub fn configure_cargo_target(wt_dir: &Path, target: &Path) -> Result<PathBuf> {
    // `.cargo/` ignored via the repo's common `info/exclude` — for a
    // linked worktree `--git-dir` is the private `worktrees/<name>`
    // dir whose excludes are never consulted, while the common file
    // covers every worktree and the main checkout alike. `git status
    // --porcelain` stays clean without touching anything tracked.
    if let Ok(gitdir) = git(wt_dir, &["rev-parse", "--git-common-dir"]) {
        let gitdir = PathBuf::from(&gitdir);
        let gitdir = if gitdir.is_absolute() {
            gitdir
        } else {
            wt_dir.join(gitdir)
        };
        let exclude = gitdir.join("info").join("exclude");
        let text = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !text.lines().any(|l| l.trim() == ".cargo/") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&exclude)
            {
                let _ = writeln!(f, ".cargo/");
            }
        }
    }
    let cargo_dir = wt_dir.join(".cargo");
    let conf = cargo_dir.join("config.toml");
    let target_str = target.to_string_lossy().into_owned();
    let mut doc: toml::Table = match std::fs::read_to_string(&conf) {
        Ok(text) => toml::from_str(&text).map_err(|e| {
            Error::rejected(format!(
                "{} is not valid TOML — fix it or remove it: {e}",
                conf.display()
            ))
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e.into()),
    };
    // An explicit target-dir the operator already wrote wins — the
    // file is still theirs; ours lands only when the key is absent.
    let existing = doc
        .get("build")
        .and_then(|b| b.get("target-dir"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    match existing.as_deref() {
        Some(existing) if existing != target_str => {
            // Record the effective dir; a relative value resolves
            // against the worktree, same as cargo resolves it.
            let effective = PathBuf::from(existing);
            Ok(if effective.is_absolute() {
                effective
            } else {
                wt_dir.join(effective)
            })
        }
        Some(_) => Ok(target.to_path_buf()),
        None => {
            let build = doc
                .entry("build".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            let build = build.as_table_mut().ok_or_else(|| {
                Error::rejected(format!("{} has a non-table [build]", conf.display()))
            })?;
            build.insert("target-dir".to_string(), toml::Value::String(target_str));
            let rendered = toml::to_string(&doc)
                .map_err(|e| Error::rejected(format!("could not write {}: {e}", conf.display())))?;
            std::fs::create_dir_all(&cargo_dir)?;
            std::fs::write(&conf, rendered)?;
            Ok(target.to_path_buf())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn project_with(build: Option<project::Build>) -> project::Project {
        project::Project {
            key: "demo".to_string(),
            prefix: "D".to_string(),
            repos: vec![],
            components: vec![],
            tags: vec![],
            default_owner: None,
            build,
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn git_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@t"]);
        git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("f"), "x").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn target_dir_defaults_to_shared() {
        let t = TempDir::new().unwrap();
        let (root, wt) = (t.path().join("repo"), t.path().join("repo/.cadence/wt/d-1"));
        for build in [
            None,
            Some(project::Build { target_dir: None }),
            Some(project::Build {
                target_dir: Some("shared".to_string()),
            }),
        ] {
            assert_eq!(
                target_dir_for(&project_with(build), &root, &wt).unwrap(),
                root.join(".cadence/target/shared")
            );
        }
    }

    #[test]
    fn target_dir_per_worktree_and_invalid() {
        let t = TempDir::new().unwrap();
        let (root, wt) = (t.path().join("repo"), t.path().join("repo/.cadence/wt/d-1"));
        let per = project_with(Some(project::Build {
            target_dir: Some("per-worktree".to_string()),
        }));
        assert_eq!(target_dir_for(&per, &root, &wt).unwrap(), wt.join("target"));
        let bad = project_with(Some(project::Build {
            target_dir: Some("/somewhere/else".to_string()),
        }));
        let e = target_dir_for(&bad, &root, &wt).unwrap_err();
        assert!(e.to_string().contains("/somewhere/else"), "{e}");
    }

    #[test]
    fn configure_writes_config_and_stays_clean() {
        let repo = git_repo();
        let target = repo.path().join(".cadence/target/shared");
        let effective = configure_cargo_target(repo.path(), &target).unwrap();
        assert_eq!(effective, target);
        let conf = std::fs::read_to_string(repo.path().join(".cargo/config.toml")).unwrap();
        assert!(
            conf.contains(&format!("target-dir = \"{}\"", target.display())),
            "{conf}"
        );
        // `.cargo/` is excluded via info/exclude — `git status` clean.
        let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
        assert!(exclude.lines().any(|l| l == ".cargo/"), "{exclude}");
        assert_eq!(git(repo.path(), &["status", "--porcelain"]), "");
        // Idempotent: a second pass changes nothing.
        assert_eq!(
            configure_cargo_target(repo.path(), &target).unwrap(),
            target
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join(".cargo/config.toml")).unwrap(),
            conf
        );
        assert_eq!(git(repo.path(), &["status", "--porcelain"]), "");
    }

    #[test]
    fn configure_preserves_operator_target() {
        let repo = git_repo();
        let cargo = repo.path().join(".cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        // An absolute operator choice wins and is recorded verbatim.
        std::fs::write(
            cargo.join("config.toml"),
            "[build]\ntarget-dir = \"/var/cache/mine\"\njobs = 2\n",
        )
        .unwrap();
        let effective =
            configure_cargo_target(repo.path(), &repo.path().join(".cadence/target/shared"))
                .unwrap();
        assert_eq!(effective, PathBuf::from("/var/cache/mine"));
        let conf = std::fs::read_to_string(cargo.join("config.toml")).unwrap();
        assert!(
            conf.contains("/var/cache/mine") && conf.contains("jobs = 2"),
            "{conf}"
        );
        // `.cargo/` was still excluded — the tree stays clean.
        assert_eq!(git(repo.path(), &["status", "--porcelain"]), "");
        // A relative operator choice resolves against the worktree.
        std::fs::write(
            cargo.join("config.toml"),
            "[build]\ntarget-dir = \"build-out\"\n",
        )
        .unwrap();
        let effective = configure_cargo_target(repo.path(), Path::new("/ignored")).unwrap();
        assert_eq!(effective, repo.path().join("build-out"));
    }

    #[test]
    fn configure_rejects_bad_toml() {
        let repo = git_repo();
        let cargo = repo.path().join(".cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        std::fs::write(cargo.join("config.toml"), "[build\nnot toml").unwrap();
        assert!(configure_cargo_target(repo.path(), Path::new("/x")).is_err());
        std::fs::write(cargo.join("config.toml"), "build = \"nope\"\n").unwrap();
        assert!(configure_cargo_target(repo.path(), Path::new("/x")).is_err());
    }
}
