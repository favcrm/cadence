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

/// The shared cargo cache for a repo — `<root>/.cadence/target/
/// shared`. One dependency-artifact store for every `.cadence/wt`
/// lane: dependency artifacts are built once per host instead of once
/// per worktree.
pub fn shared_target_dir(root: &Path) -> PathBuf {
    root.join(".cadence").join("target").join("shared")
}

/// The `debug/` children cargo fills with *hashed* names —
/// `<name>-<metadata>.<ext>` keyed by package id (which includes the
/// source path), features and profile — so two lanes' artifacts never
/// share a filename. These are the dirs a worktree symlinks into the
/// shared cache; cargo's build-lock files join them so concurrent
/// lanes queue on cargo's own locking. Everything else under `debug/` —
/// uplifted binaries, uplifted rlibs, `.d` files — is unhashed and
/// stays per-lane, which is what keeps one lane's `cargo test` from
/// exec'ing another lane's `debug/cadence`.
const SHARED_DEBUG_DIRS: [&str; 5] = ["deps", ".fingerprint", "build", "incremental", "examples"];
/// Cargo's build locks (all three exist on modern toolchains) are
/// shared too, so two lanes building at once serialise on cargo's own
/// locking rather than racing writes into the shared `deps/`.
const SHARED_DEBUG_FILES: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// Should the project's worktrees share the dep cache?
/// `build: {target_dir: per-worktree}` in `project.yaml` opts a lane
/// back onto fully-private build output; anything else is rejected.
pub fn shared_deps_enabled(project: &project::Project) -> Result<bool> {
    match project.build.as_ref().and_then(|b| b.target_dir.as_deref()) {
        None | Some("shared") => Ok(true),
        Some("per-worktree") => Ok(false),
        Some(other) => Err(Error::rejected(format!(
            "[build] target_dir = \"{other}\" in {}'s project.yaml — \
             expected \"shared\" or \"per-worktree\"",
            project.key
        ))),
    }
}

/// Where cargo will actually put this worktree's build output: an
/// explicit `build.target-dir` in the worktree's own
/// `.cargo/config.toml` wins (relative values resolve against the
/// worktree, same as cargo resolves them); otherwise the default
/// `<wt>/target`. The worktree-local file is the only one cadence
/// inspects — a `CARGO_TARGET_DIR` env or a config higher in cargo's
/// chain overrides the same way it always has; the ref records the
/// best-known effective dir.
pub fn effective_target_dir(wt_dir: &Path) -> PathBuf {
    let conf = wt_dir.join(".cargo").join("config.toml");
    if let Ok(text) = std::fs::read_to_string(&conf) {
        if let Ok(doc) = toml::from_str::<toml::Table>(&text) {
            if let Some(dir) = doc
                .get("build")
                .and_then(|b| b.get("target-dir"))
                .and_then(|v| v.as_str())
            {
                let dir = PathBuf::from(dir);
                return if dir.is_absolute() {
                    dir
                } else {
                    wt_dir.join(dir)
                };
            }
        }
    }
    wt_dir.join("target")
}

/// Move every entry of `src` into `dst` (same filesystem — both live
/// under the repo), skipping names already present in `dst`, then
/// remove `src`. Folds a lane's existing real `deps/` into the shared
/// cache before symlinking.
fn merge_dir_into(src: &Path, dst: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest = dst.join(entry.file_name());
        if !dest.exists() {
            std::fs::rename(entry.path(), &dest)?;
        }
    }
    std::fs::remove_dir_all(src)?;
    Ok(())
}

/// Is `link` a symlink pointing at `target`?
fn is_link_to(link: &Path, target: &Path) -> bool {
    link.symlink_metadata()
        .ok()
        .filter(|m| m.file_type().is_symlink())
        .is_some_and(|_| std::fs::read_link(link).is_ok_and(|t| t == target))
}

/// Is `path` under an exclusive flock right now? A non-blocking
/// LOCK_EX attempt — success means free, and the `File` drop releases
/// the probe lock immediately.
pub(crate) fn file_locked(path: &Path) -> bool {
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    use std::os::unix::io::AsRawFd;
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
}

/// Point a worktree's cargo builds at the shared dep cache: inside
/// `<wt>/target/debug`, the hashed-content subdirs become symlinks to
/// `<root>/.cadence/target/shared/debug/<name>` (merging any existing
/// real dir first), while the lane's own `debug/` stays a real dir —
/// uplifted binaries like `debug/cadence` are per-lane files, so one
/// lane's `cargo test` can never exec another lane's binary. With
/// `shared=false` a previously-planted farm is undone (links into our
/// shared dir become empty real dirs) so a `per-worktree` lane is
/// fully private again. Nothing under `.cargo/` is written or
/// excluded — a tracked or hand-written `.cargo/config.toml` is never
/// touched. Returns the effective target dir for the worktree ref.
pub fn configure_cargo_target(wt_dir: &Path, root: &Path, shared: bool) -> Result<PathBuf> {
    let effective = effective_target_dir(wt_dir);
    let wt_target = wt_dir.join("target");
    // An operator's `build.target-dir` redirects cargo elsewhere —
    // the farm would sit inert, so it is not planted.
    if effective != wt_target {
        return Ok(effective);
    }
    let wt_debug = wt_target.join("debug");
    let shared_debug = shared_target_dir(root).join("debug");
    for name in SHARED_DEBUG_DIRS {
        let link = wt_debug.join(name);
        let shared_sub = shared_debug.join(name);
        if shared {
            std::fs::create_dir_all(&shared_sub)?;
            std::fs::create_dir_all(&wt_debug)?;
            if is_link_to(&link, &shared_sub) {
                continue;
            }
            if link.is_dir() && !link.is_symlink() {
                // A real dir from a pre-shared build — fold its
                // artifacts into the cache, then link.
                merge_dir_into(&link, &shared_sub)?;
            } else if link.exists() || link.is_symlink() {
                // A symlink to somewhere else is the operator's.
                continue;
            }
            std::os::unix::fs::symlink(&shared_sub, &link)?;
        } else if is_link_to(&link, &shared_sub) {
            std::fs::remove_file(&link)?;
            std::fs::create_dir_all(&link)?;
        }
    }
    for name in SHARED_DEBUG_FILES {
        let link = wt_debug.join(name);
        let shared_file = shared_debug.join(name);
        if shared {
            std::fs::create_dir_all(&wt_debug)?;
            if is_link_to(&link, &shared_file) || link.is_symlink() {
                continue;
            }
            if link.exists() {
                // A real lock file from a pre-shared build joins the
                // shared lock — unless a build holds it right now.
                if file_locked(&link) {
                    continue;
                }
                std::fs::remove_file(&link)?;
            }
            std::os::unix::fs::symlink(&shared_file, &link)?;
        } else if is_link_to(&link, &shared_file) {
            std::fs::remove_file(&link)?;
        }
    }
    Ok(effective)
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
        std::fs::write(dir.path().join(".gitignore"), "/target\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn shared_deps_default_and_per_worktree() {
        for build in [
            None,
            Some(project::Build { target_dir: None }),
            Some(project::Build {
                target_dir: Some("shared".to_string()),
            }),
        ] {
            assert!(shared_deps_enabled(&project_with(build)).unwrap());
        }
        let per = project_with(Some(project::Build {
            target_dir: Some("per-worktree".to_string()),
        }));
        assert!(!shared_deps_enabled(&per).unwrap());
        let bad = project_with(Some(project::Build {
            target_dir: Some("/somewhere/else".to_string()),
        }));
        let e = shared_deps_enabled(&bad).unwrap_err();
        assert!(e.to_string().contains("/somewhere/else"), "{e}");
    }

    #[test]
    fn configure_plants_farm_and_stays_clean() {
        let repo = git_repo();
        let wt = repo.path();
        let effective = configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(effective, wt.join("target"));
        let debug = wt.join("target/debug");
        for name in SHARED_DEBUG_DIRS {
            let link = debug.join(name);
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                shared_target_dir(repo.path()).join("debug").join(name),
                "{name}"
            );
        }
        for name in SHARED_DEBUG_FILES {
            let link = debug.join(name);
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                shared_target_dir(repo.path()).join("debug").join(name),
                "{name}"
            );
        }
        // `debug/` itself is a real dir — uplifted binaries are per-lane.
        assert!(!debug.is_symlink());
        // No `.cargo/` anywhere — a tracked config would be untouched.
        assert!(!wt.join(".cargo").exists());
        assert_eq!(git(wt, &["status", "--porcelain"]), "");
        // Idempotent.
        configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(git(wt, &["status", "--porcelain"]), "");
    }

    #[test]
    fn configure_merges_existing_target_into_shared() {
        let repo = git_repo();
        let wt = repo.path();
        // A pre-shared lane already built: real dirs with artifacts.
        let debug = wt.join("target/debug");
        std::fs::create_dir_all(debug.join("deps")).unwrap();
        std::fs::write(debug.join("deps/libdep-abc.rlib"), "rlib").unwrap();
        std::fs::create_dir_all(debug.join(".fingerprint/dep-abc")).unwrap();
        std::fs::write(debug.join("probe"), "bin").unwrap();
        configure_cargo_target(wt, repo.path(), true).unwrap();
        let shared = shared_target_dir(repo.path()).join("debug");
        // Artifacts moved into the cache; the lane's own bin stayed.
        assert_eq!(
            std::fs::read_to_string(shared.join("deps/libdep-abc.rlib")).unwrap(),
            "rlib"
        );
        assert!(shared.join(".fingerprint/dep-abc").is_dir());
        assert_eq!(std::fs::read_to_string(debug.join("probe")).unwrap(), "bin");
        assert!(debug.join("deps").is_symlink());
    }

    #[test]
    fn configure_per_worktree_unplants_farm() {
        let repo = git_repo();
        let wt = repo.path();
        configure_cargo_target(wt, repo.path(), true).unwrap();
        let effective = configure_cargo_target(wt, repo.path(), false).unwrap();
        assert_eq!(effective, wt.join("target"));
        for name in SHARED_DEBUG_DIRS {
            let p = wt.join("target/debug").join(name);
            assert!(p.is_dir() && !p.is_symlink(), "{name}");
        }
        for name in SHARED_DEBUG_FILES {
            assert!(!wt.join("target/debug").join(name).exists(), "{name}");
        }
        // The shared cache kept its dirs — nothing was deleted.
        assert!(shared_target_dir(repo.path()).join("debug/deps").is_dir());
    }

    #[test]
    fn configure_preserves_operator_target_dir_and_config() {
        let repo = git_repo();
        let wt = repo.path();
        let cargo = wt.join(".cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        let conf_text = "[build]\ntarget-dir = \"/var/cache/mine\"\njobs = 2\n";
        std::fs::write(cargo.join("config.toml"), conf_text).unwrap();
        git(wt, &["add", "-A"]);
        git(wt, &["commit", "-qm", "cargo config"]);
        let effective = configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(effective, PathBuf::from("/var/cache/mine"));
        // Tracked config survives byte-for-byte; no farm planted.
        assert_eq!(
            std::fs::read_to_string(cargo.join("config.toml")).unwrap(),
            conf_text
        );
        assert!(!wt.join("target").exists());
        assert_eq!(git(wt, &["status", "--porcelain"]), "");
        // A relative operator choice resolves against the worktree.
        std::fs::write(
            cargo.join("config.toml"),
            "[build]\ntarget-dir = \"build-out\"\n",
        )
        .unwrap();
        let effective = configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(effective, wt.join("build-out"));
        assert!(!wt.join("target").exists());
    }

    #[test]
    fn configure_leaves_foreign_symlinks() {
        let repo = git_repo();
        let wt = repo.path();
        let foreign = wt.join("elsewhere");
        std::fs::create_dir_all(&foreign).unwrap();
        let debug = wt.join("target/debug");
        std::fs::create_dir_all(&debug).unwrap();
        std::os::unix::fs::symlink(&foreign, debug.join("deps")).unwrap();
        configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(std::fs::read_link(debug.join("deps")).unwrap(), foreign);
    }
}
