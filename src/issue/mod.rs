//! The PM board: issue folders under a private directory (`~/pm` or
//! `CADENCE_PM_DIR`) that is its own git repo — `issue init` installs
//! hooks that lint each commit and push the private `origin` remote.
//! The `cadence issue` CLI is the only writer; `cadence ui` is a
//! read/write front end over the same files plus agent-notes and the
//! daemon socket.

pub mod app;
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
pub mod workflow;
pub mod write;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Default tracker directory: `$CADENCE_PM_DIR`, else the resolved
/// home's `tracker/` — `~/pm` under the legacy layout
/// ([`crate::home`]).
pub fn default_dir() -> Result<PathBuf> {
    crate::home::tracker_dir()
}

/// The tracker `default_dir` resolves with `CADENCE_PM_DIR` unset —
/// `<home>/tracker`, or `~/pm` under the legacy layout: the
/// production default a sandbox must never reach.
pub fn home_default_dir() -> Result<PathBuf> {
    crate::home::tracker_default()
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
    /// CAD-538: the hosting daemon's lease, attached by `Shared::pm`.
    /// When set, `lock`/`try_lock`/`commit` refuse the moment the
    /// lease's fence trips, and commits carry a `Lease-Epoch:` trailer.
    /// `None` for CLI/local use — nothing there changes.
    lease: Option<crate::lease::PmLease>,
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
            lease: None,
        })
    }

    /// CAD-538: attach the daemon's lease — `lock`/`try_lock`/`commit`
    /// then refuse once the lease fence trips and `commit` stamps the
    /// epoch. `pub(crate)`: only daemon code attaches; the CLI stays
    /// lease-free.
    pub(crate) fn attach_lease(&mut self, lease: crate::lease::PmLease) {
        self.lease = Some(lease);
    }

    /// The lease gate every tracker write passes: refusal while the
    /// fence is tripped, pass-through otherwise (and always when the
    /// handle carries no lease — CLI, tests, unleased daemons).
    fn fence_check(&self) -> Result<()> {
        match &self.lease {
            Some(lease) => lease.check(),
            None => Ok(()),
        }
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
        self.fence_check()?;
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
        self.fence_check()?;
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
    ///
    /// CAD-538: under a daemon lease the commit is refused once the
    /// fence trips — checked again here so a `PmLock` taken before the
    /// loss cannot sneak a commit past it — and the message gains a
    /// `Lease-Epoch:` trailer naming the writer generation.
    pub fn commit(&self, paths: &[PathBuf], message: &str) -> Result<Vec<String>> {
        self.fence_check()?;
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
        // CAD-538: the lease epoch rides every commit a leased daemon
        // makes — a reader can order writes across restarts and
        // holdovers. The trailer joins the closing block directly, or
        // starts one after a blank line when the message ends bare.
        let message = match &self.lease {
            Some(lease) => {
                let body = message.trim_end_matches('\n');
                let trailer_shaped = body.rsplit('\n').next().is_some_and(|l| l.contains(": "));
                format!(
                    "{body}{}Lease-Epoch: {}\n",
                    if trailer_shaped { "\n" } else { "\n\n" },
                    lease.epoch()
                )
            }
            None => message,
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

    /// CAD-538 — the tracker half of the SIGTERM flush: commit whatever
    /// the index already stages, the pending write a crash or kill left
    /// mid-flight. Never `git add` — worktree dirt and unstaged edits
    /// are not this flush's to claim, and a planted file somebody only
    /// dropped must not ride in. `Ok(false)` when nothing was staged or
    /// the tracker lock was busy. A leased daemon that has lost its
    /// lease is refused here like every other write.
    pub fn flush_pending(&self, actor: &str) -> Result<bool> {
        self.fence_check()?;
        let Some(_lock) = self.try_lock()? else {
            return Ok(false);
        };
        // exit 1 = the index holds staged changes; 0 = clean.
        let staged = crate::reaper::status(
            Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(["diff", "--cached", "--quiet"]),
        )
        .map(|s| !s.success())
        .unwrap_or(false);
        if !staged {
            return Ok(false);
        }
        let mut message = format!("cadence flush on stop\n\nActor: {actor}\n");
        if let Some(lease) = &self.lease {
            message.push_str(&format!("Lease-Epoch: {}\n", lease.epoch()));
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
                message.as_str(),
            ],
        )?;
        Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// CAD-538: a `Pm` carrying the daemon's lease stamps every commit
    /// `Lease-Epoch:` and refuses every write the moment the fence
    /// trips — lock, try_lock, commit and the shutdown flush alike.
    #[test]
    fn cad538_leased_pm_fences_writes_and_stamps_epoch() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let pm_dir = dir.path().join("pm");
        let mut pm = Pm::init(&pm_dir).unwrap();
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

        // While the lease holds, writes land and commits carry the epoch.
        let note = pm_dir.join("note.md");
        std::fs::write(&note, "one\n").unwrap();
        pm.commit(std::slice::from_ref(&note), "test write\n")
            .unwrap();
        let log = git(&pm_dir, &["log", "-1", "--format=%B"]).unwrap();
        assert!(log.contains("Lease-Epoch: 1"), "{log}");
        assert!(pm.lock().is_ok());
        assert!(pm.try_lock().unwrap().is_some());

        // Lease loss fences every write path, before anything moves.
        ctl.fence().trip("test lease loss");
        std::fs::write(&note, "two\n").unwrap();
        let e = pm
            .commit(std::slice::from_ref(&note), "stolen\n")
            .unwrap_err();
        assert!(e.to_string().contains("lease"), "{e}");
        assert!(pm.lock().is_err());
        assert!(pm.try_lock().is_err());
        assert!(pm.flush_pending("test").is_err());
        // Nothing of the refused write staged — no partial write.
        let staged = git(&pm_dir, &["diff", "--cached", "--name-only"]).unwrap();
        assert_eq!(staged, "", "the refused write staged: {staged}");
        // The refused commit added no object: HEAD is still the
        // `test write` commit.
        let head = git(&pm_dir, &["log", "-1", "--format=%s"]).unwrap();
        assert_eq!(head, "test write", "{head}");
    }

    /// CAD-538 r2: expiry is the second tripwire — a lease that lapses
    /// while the daemon still runs (a shutdown tail outliving the TTL)
    /// fences every write with no renewal failure at all.
    #[test]
    fn cad538_expired_lease_fences_writes() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let pm_dir = dir.path().join("pm");
        let mut pm = Pm::init(&pm_dir).unwrap();
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

        // Never tripped — but expired: the write paths refuse alike.
        ctl.fence().set_expiry(0.0);
        let note = pm_dir.join("note.md");
        std::fs::write(&note, "two\n").unwrap();
        let e = pm
            .commit(std::slice::from_ref(&note), "post-expiry\n")
            .unwrap_err();
        assert!(e.to_string().contains("expired"), "{e}");
        assert!(pm.lock().is_err());
        assert!(pm.flush_pending("test").is_err());
        let staged = git(&pm_dir, &["diff", "--cached", "--name-only"]).unwrap();
        assert_eq!(staged, "", "the refused write staged: {staged}");
    }
}
