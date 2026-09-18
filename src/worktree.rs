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
