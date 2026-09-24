//! The PM board: issue folders under a private directory (`~/pm` or
//! `CADENCE_PM_DIR`) that is its own git repo — `issue init` installs
//! hooks that lint each commit and push the private `origin` remote.
//! The `cadence issue` CLI is the only writer; `cadence ui` is a
//! read/write front end over the same files plus agent-notes and the
//! daemon socket.

pub mod areas;
pub mod board;
pub mod claim;
pub mod cli;
pub mod context;
pub mod dispatch;
pub mod doctor;
pub mod finish;
pub mod history;
pub mod hooks;
pub mod line_times;
pub mod lint;
pub mod model;
pub mod notes;
pub mod parse;
pub mod plan;
pub mod project;
pub mod project_new;
pub mod relay;
pub mod report;
pub mod retro;
pub mod start;
pub mod summary;
pub mod sync;
pub mod task_report;
pub mod time;
pub mod work;
pub mod write;

use std::collections::HashSet;
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
    home_default_dir()
}

/// The tracker `default_dir` resolves with `CADENCE_PM_DIR` unset —
/// `~/pm`, the production default a sandbox must never reach.
pub fn home_default_dir() -> Result<PathBuf> {
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
            let _ = pm.commit(
                &[pm_yaml, readme, gitignore],
                &format!("init\n\nActor: {}", write::actor_who("", None)),
            )?;
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

    /// [`Self::lock`] without the wait: `None` when another writer holds
    /// it. For a caller that must not stall behind a writer (the daemon
    /// under its own lock, CAD-449) and retries later instead.
    pub fn try_lock(&self) -> Result<Option<PmLock>> {
        let path = self.dir.join(".write.lock");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => Ok(Some(PmLock { path })),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// True when the worktree differs from HEAD.
    fn git_dirty(&self) -> bool {
        let status = crate::reaper::output(
            Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(["status", "--porcelain"]),
        );
        match status {
            Ok(out) => out.status.success() && !out.stdout.is_empty(),
            Err(_) => false,
        }
    }

    /// `git add -- <paths>` + one path-limited `git commit -- <paths>`
    /// — every CLI write ends in exactly one commit, and that commit
    /// carries only the paths the write itself touched. `git add -A`
    /// swept any planted file (a forged report, a stray note) into the
    /// next ordinary write under that writer's name (CAD-454); a
    /// path-limited commit never does, even when foreign entries are
    /// already staged. The identity is fixed so PM commits never
    /// depend on user config.
    ///
    /// Foreign paths — staged, modified or untracked files this write
    /// did not touch — are left alone and reported once: stderr for
    /// the operator watching the CLI/daemon log, the commit's
    /// `Foreign-Files:` trailer for the audit trail, and the returned
    /// list for callers that surface JSON. They never block the write
    /// (a plant must not become a way to stall every tracker write).
    /// On an add or commit failure this write's own paths are
    /// unstaged, so a retry — or the next writer — never carries them.
    ///
    /// `paths` are absolute (or `pm.dir`-relative) paths under the
    /// tracker; an empty list or a path outside `pm.dir` is a caller
    /// bug and refused. Returns the foreign paths, repo-relative.
    pub fn commit(&self, paths: &[PathBuf], message: &str) -> Result<Vec<String>> {
        let mut rel = Vec::with_capacity(paths.len());
        for p in paths {
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                self.dir.join(p)
            };
            let r = abs.strip_prefix(&self.dir).map_err(|_| {
                Error::internal(format!(
                    "tracker write {} is outside the PM dir {}",
                    p.display(),
                    self.dir.display()
                ))
            })?;
            rel.push(r.to_string_lossy().into_owned());
        }
        if rel.is_empty() {
            return Err(Error::internal(
                "a tracker commit must name the paths it wrote",
            ));
        }
        rel.sort();
        rel.dedup();
        let unstage = |dir: &Path, rel: &[String]| {
            let mut args = vec!["reset", "-q", "--"];
            args.extend(rel.iter().map(String::as_str));
            let _ = git(dir, &args);
        };
        {
            let mut add = vec!["add", "--"];
            add.extend(rel.iter().map(String::as_str));
            if let Err(e) = git(&self.dir, &add) {
                unstage(&self.dir, &rel);
                return Err(e);
            }
        }
        let (foreign, foreign_extra) = self.foreign_paths(&rel);
        // Nothing of ours staged means nothing to commit — the foreign
        // paths still get reported.
        if !self.staged_dirty(&rel) {
            warn_foreign(&self.dir, &foreign, foreign_extra);
            return Ok(foreign_out(foreign, foreign_extra));
        }
        let message = if foreign.is_empty() && foreign_extra == 0 {
            message.to_string()
        } else {
            format!(
                "{message}Foreign-Files: {}\n",
                foreign_listed(&foreign, foreign_extra)
            )
        };
        let mut commit = vec![
            "-c",
            "user.name=cadence",
            "-c",
            "user.email=cadence@localhost",
            "commit",
            "-q",
            "-m",
            message.as_str(),
            "--",
        ];
        commit.extend(rel.iter().map(String::as_str));
        if let Err(e) = git(&self.dir, &commit) {
            unstage(&self.dir, &rel);
            return Err(e);
        }
        warn_foreign(&self.dir, &foreign, foreign_extra);
        Ok(foreign_out(foreign, foreign_extra))
    }

    /// True when the index carries a change under one of `rel`.
    fn staged_dirty(&self, rel: &[String]) -> bool {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.dir)
            .args(["diff", "--cached", "--quiet", "--"]);
        cmd.args(rel);
        // exit 1 = staged changes under those paths; 0 = clean.
        crate::reaper::status(&mut cmd)
            .map(|s| !s.success())
            .unwrap_or(false)
    }

    /// Paths `git status` reports that this write did not touch —
    /// untracked plants, foreign modifications and staged entries
    /// alike. `--untracked-files=all` so a plant inside a fresh
    /// directory is named exactly (`?? dir/` collapsing would report
    /// only the directory — useless for spotting a forged file).
    /// `.write.lock` and `.index/` are the tracker's own scratch
    /// (gitignored); they are filtered so a missing .gitignore never
    /// turns every write into a false alarm.
    ///
    /// Returns (paths, extra): `paths` holds at most [`FOREIGN_CAP`]
    /// entries and `extra` counts the rest — a planted tree cannot
    /// make a write collect and report unbounded lists. git still
    /// walks the tree once; that walk is the price of a correct scan.
    fn foreign_paths(&self, ours: &[String]) -> (Vec<String>, usize) {
        // Raw stdout — porcelain's leading-space XY codes must not be
        // trimmed (` M` is an unstaged modification).
        let Ok(out) = crate::reaper::output(Command::new("git").arg("-C").arg(&self.dir).args([
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
        ])) else {
            return (vec![], 0);
        };
        if !out.status.success() {
            return (vec![], 0);
        }
        let out = String::from_utf8_lossy(&out.stdout);
        let ours: HashSet<&str> = ours.iter().map(String::as_str).collect();
        let foreign =
            |p: &str| !ours.contains(p) && p != ".write.lock" && !p.starts_with(".index/");
        let mut found = Vec::new();
        let mut extra = 0usize;
        let mut note = |path: &str| {
            if !foreign(path) {
                return;
            }
            if found.len() < FOREIGN_CAP {
                found.push(path.to_string());
            } else {
                extra += 1;
            }
        };
        let mut fields = out.split('\0');
        while let Some(rec) = fields.next() {
            if rec.len() < 4 || rec.as_bytes()[2] != b' ' {
                continue;
            }
            let (xy, path) = rec.split_at(2);
            note(&path[1..]);
            // `-z` rename/copy entries carry a second field — the
            // source path — with no status prefix of its own.
            if xy.contains('R') || xy.contains('C') {
                if let Some(orig) = fields.next() {
                    note(orig);
                }
            }
        }
        found.sort();
        found.dedup();
        (found, extra)
    }
}

/// The most foreign paths a write collects and reports — beyond it a
/// trailing "(+N more)" stands in everywhere the list is surfaced.
const FOREIGN_CAP: usize = 64;

/// `foreign` + its unlisted `extra` as the value callers serialize:
/// the marker travels with the list so JSON results show the same
/// truncation the operator sees on stderr and in the commit trailer.
fn foreign_out(mut foreign: Vec<String>, extra: usize) -> Vec<String> {
    if extra > 0 {
        foreign.push(format!("(+{extra} more)"));
    }
    foreign
}

/// One stderr line per write that saw foreign files — the immediate
/// signal for the operator watching the CLI or the daemon's log.
fn warn_foreign(dir: &Path, foreign: &[String], extra: usize) {
    if foreign.is_empty() && extra == 0 {
        return;
    }
    eprintln!(
        "warning: {} foreign path(s) under {} left uncommitted: {}",
        foreign.len() + extra,
        dir.display(),
        foreign_listed(foreign, extra)
    );
}

/// The foreign-path list as one line — capped so a planted tree cannot
/// blow up a commit message or a log line.
fn foreign_listed(foreign: &[String], extra: usize) -> String {
    const SHOW: usize = 8;
    let mut listed: Vec<String> = foreign
        .iter()
        .take(SHOW)
        .map(|p| {
            p.chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .take(160)
                .collect()
        })
        .collect();
    let hidden = foreign.len().saturating_sub(SHOW) + extra;
    if hidden > 0 {
        listed.push(format!("(+{hidden} more)"));
    }
    listed.join(", ")
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
    let out = crate::reaper::output(Command::new("git").arg("-C").arg(dir).args(args))
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
- The only writer is `cadence issue` (`new set link unlink ref comment attach acceptance`).\n\
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
- Acceptance criteria are authored with cadence issue acceptance <ID>\n\
  --from <file> as an ordered - [ ]/- [x] checklist; dispatch enforcement\n\
  remains a later CAD-159 change.\n\
- Every write is one git commit, serialised on `.write.lock`.\n";
