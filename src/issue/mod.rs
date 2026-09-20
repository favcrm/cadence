//! The PM board: issue folders under a private directory (`~/pm` or
//! `CADENCE_PM_DIR`) that is its own git repo — `issue init` installs
//! hooks that lint each commit and push the private `origin` remote.
//! The `cadence issue` CLI is the only writer; `cadence ui` is a
//! read/write front end over the same files plus agent-notes and the
//! daemon socket.

pub mod board;
pub mod cli;
pub mod dispatch;
pub mod doctor;
pub mod finish;
pub mod history;
pub mod hooks;
pub mod lint;
pub mod model;
pub mod notes;
pub mod parse;
pub mod project;
pub mod report;
pub mod start;
pub mod sync;
pub mod time;
pub mod write;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Default tracker directory: `$CADENCE_PM_DIR`, else `~/pm`.
pub fn default_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CADENCE_PM_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| Error::rejected("HOME is not set to an absolute path"))?;
    Ok(home.join("pm"))
}

/// `pm.yaml` — schema version, vocabularies, limits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PmConfig {
    pub schema: u32,
    #[serde(default = "default_statuses")]
    pub statuses: Vec<String>,
    #[serde(default = "default_link_types")]
    pub link_types: Vec<String>,
    #[serde(default = "default_artifact_cap")]
    pub artifact_max_bytes: u64,
    #[serde(default = "default_notes_dir")]
    pub notes_dir: String,
}

fn default_statuses() -> Vec<String> {
    model::STATUSES.iter().map(|s| s.to_string()).collect()
}
fn default_link_types() -> Vec<String> {
    model::LINK_KINDS.iter().map(|s| s.to_string()).collect()
}
fn default_artifact_cap() -> u64 {
    1_048_576
}
fn default_notes_dir() -> String {
    "/var/www/agent-notes".to_string()
}

impl Default for PmConfig {
    fn default() -> Self {
        Self {
            schema: 1,
            statuses: default_statuses(),
            link_types: default_link_types(),
            artifact_max_bytes: default_artifact_cap(),
            notes_dir: default_notes_dir(),
        }
    }
}

impl PmConfig {
    pub fn notes_dir(&self) -> PathBuf {
        project::expand_home(&self.notes_dir)
    }
}

/// A handle on the tracker directory.
pub struct Pm {
    pub dir: PathBuf,
    pub config: PmConfig,
}

impl Pm {
    /// Open an existing PM dir (requires `pm.yaml`).
    pub fn at(dir: &Path) -> Result<Pm> {
        let file = dir.join("pm.yaml");
        if !file.is_file() {
            return Err(Error::rejected(format!(
                "No PM dir at {} — create it with `cadence issue init`",
                dir.display()
            )));
        }
        let text = std::fs::read_to_string(&file)?;
        let config: PmConfig = serde_yaml::from_str(&text).map_err(|e| {
            Error::internal(format!("{} is not valid pm.yaml: {e}", file.display()))
        })?;
        Ok(Pm {
            dir: dir.to_path_buf(),
            config,
        })
    }

    pub fn open_default() -> Result<Pm> {
        Self::at(&default_dir()?)
    }

    /// `issue init` — idempotent skeleton: pm.yaml, README, .gitignore,
    /// `git init` and the first commit.
    pub fn init(dir: &Path) -> Result<Pm> {
        std::fs::create_dir_all(dir)?;
        let pm_yaml = dir.join("pm.yaml");
        if !pm_yaml.exists() {
            let config = PmConfig::default();
            let text = serde_yaml::to_string(&config)
                .map_err(|e| Error::internal(format!("pm.yaml: {e}")))?;
            std::fs::write(&pm_yaml, text)?;
        }
        let readme = dir.join("README.md");
        if !readme.exists() {
            std::fs::write(&readme, README)?;
        }
        let gitignore = dir.join(".gitignore");
        if !gitignore.exists() {
            std::fs::write(&gitignore, ".write.lock\n.index/\n")?;
        }
        if !dir.join(".git").exists() {
            git(dir, &["init", "-q"])?;
        }
        let pm = Self::at(dir)?;
        // Every write is a commit — the skeleton included.
        if pm.git_dirty() {
            pm.commit(&format!("init\n\nActor: {}", write::actor_who("", None)))?;
        }
        Ok(pm)
    }

    /// Serialise writers: comments/artifacts are create-only and ids are
    /// allocated under this same lock. Lock file, create-exclusive,
    /// bounded spin; the holder removes it on drop.
    pub fn lock(&self) -> Result<PmLock> {
        let path = self.dir.join(".write.lock");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(PmLock { path: path.clone() }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(Error::rejected(format!(
                            "PM dir is locked by another writer ({}); remove it \
                             only if the holder is gone",
                            path.display()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// True when the worktree differs from HEAD.
    fn git_dirty(&self) -> bool {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["status", "--porcelain"])
            .output();
        match status {
            Ok(out) => out.status.success() && !out.stdout.is_empty(),
            Err(_) => false,
        }
    }

    /// `git add -A` + one commit — every CLI write ends in exactly one.
    /// The identity is fixed so PM commits never depend on user config.
    pub fn commit(&self, message: &str) -> Result<()> {
        git(&self.dir, &["add", "-A"])?;
        if !self.git_dirty_cached() {
            return Ok(());
        }
        git(
            &self.dir,
            &[
                "-c",
                "user.name=cadence",
                "-c",
                "user.email=cadence@localhost",
                "commit",
                "-q",
                "-m",
                message,
            ],
        )?;
        Ok(())
    }

    fn git_dirty_cached(&self) -> bool {
        let diff = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["diff", "--cached", "--quiet"])
            .status();
        match diff {
            // exit 1 = staged changes exist; 0 = clean.
            Ok(s) => !s.success(),
            Err(_) => false,
        }
    }
}

pub struct PmLock {
    path: PathBuf,
}

impl Drop for PmLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Run a git subcommand in `dir`; rejected error carries stderr.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

const README: &str = "# ~/pm — the cadence board\n\n\
One folder per issue: `<project>/<ID>/issue.md` + `comments/` + `artifacts/`.\n\
Paths never encode title, status or parent. Issues are never deleted — set\n\
`status: dropped`.\n\n\
## Rules for agents and humans\n\n\
- The only writer is `cadence issue` (`new set link unlink ref comment attach`).\n\
  Hand-edits are legal but must be followed by `cadence issue lint`.\n\
- Statuses: `backlog ready doing review done dropped`.\n\
- Links live on one side only (`blocked_by parent relates duplicate_of`);\n\
  inverses are computed. Only `blocked_by` gates readiness.\n\
- Sub-issues carry `parent`, depth is two levels, and a parent with\n\
  children is a container: never dispatched, status rolls up.\n\
- Derived status wins over the file field: roll-up first, then the newest\n\
  agent-note whose header has `Issue: <ID>`, then the file value.\n\
- Refs point at PRs/commits/notes/previews/messages/urls. Files go in\n\
  `artifacts/` under `artifact_max_bytes`. The listing is the manifest.\n\
- Comments are one file each, create-only, named by UTC time + author.\n\
- Every write is one git commit, serialised on `.write.lock`.\n";
