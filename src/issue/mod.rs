//! The PM board: issue folders under a private directory (`~/pm` or
//! `CADENCE_PM_DIR`) that is its own git repo — `issue init` installs
//! hooks that lint each commit and push the private `origin` remote.
//! The `cadence issue` CLI is the only writer; `cadence ui` is a
//! read/write front end over the same files plus agent-notes and the
//! daemon socket.

pub mod app;
pub mod app_access;
pub mod app_action;
pub mod app_assistant;
pub mod app_campaign_work;
pub mod app_catalog;
pub mod app_chat;
pub mod app_guard;
pub mod app_listing;
pub mod app_screen_decl;
pub mod app_screen_pkg;
pub mod app_source;
pub mod app_view;
pub mod areas;
pub mod blocked;
pub mod board;
#[cfg(test)]
mod cad811_screen_startup_acceptance;
pub mod claim;
pub mod cli;
pub mod cloud;
pub mod context;
pub mod delivery_policy;
pub mod delivery_requirements;
pub mod dispatch;
pub mod doctor;
pub mod edit;
pub mod finish;
pub mod groom;
pub mod history;
pub mod hooks;
pub mod idea;
pub mod line_times;
pub mod lint;
pub(crate) mod lockseam;
pub mod model;
pub mod notes;
pub mod parse;
pub mod plan;
mod pmlock;
pub(crate) mod process_use;
pub mod project;
pub mod project_new;
pub mod reclaim;
pub mod reconcile;
pub mod recovery;
pub mod relay;
pub mod report;
pub mod retro;
pub mod sprint;
pub mod start;
pub mod summary;
pub mod sync;
pub mod task_report;
pub mod time;
pub mod work;
pub mod workflow;
pub mod write;

pub use pmlock::{InterruptionReport, LockState, PmLock};

/// The legacy lock path, kept as the new protocol's fence file.
const MARKER_LOCK_FILE: &str = ".write.lock";

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    /// CAD-580: the wiki store's upload cap (`wiki_put_blob` refuses a
    /// file over it before the blob lands).
    #[serde(default)]
    pub wiki: WikiConfig,
    /// CAD-362: the operator's reviewer-pairing catalog — `pair` pins
    /// an author's reviews to one reviewer, `never` bars a pair.
    #[serde(default)]
    pub review: crate::delivery::ReviewRules,
    /// CAD-1218: who may record a merge approval from a remote board.
    /// Omitted from a fresh pm.yaml, so an operator's own `approvals:`
    /// block never collides with a written default.
    #[serde(default, skip_serializing_if = "ApprovalsConfig::is_empty")]
    pub approvals: ApprovalsConfig,
}

/// `pm.yaml`'s `approvals:` section (CAD-1218): the tailnet logins the
/// operator allows to approve a PR head from the board. Absent or empty
/// means no remote approvals; the loopback board is always allowed.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ApprovalsConfig {
    #[serde(default)]
    pub tailnet_logins: Vec<String>,
}

impl ApprovalsConfig {
    fn is_empty(&self) -> bool {
        self.tailnet_logins.is_empty()
    }
}

/// `pm.yaml`'s `wiki:` section.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WikiConfig {
    /// Largest accepted blob upload, bytes (default 100 MiB).
    #[serde(default = "default_wiki_upload_cap")]
    pub max_upload_bytes: u64,
}

impl Default for WikiConfig {
    fn default() -> Self {
        Self {
            max_upload_bytes: default_wiki_upload_cap(),
        }
    }
}

fn default_wiki_upload_cap() -> u64 {
    100 * 1024 * 1024
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
            wiki: WikiConfig::default(),
            review: crate::delivery::ReviewRules::default(),
            approvals: ApprovalsConfig::default(),
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
    /// CAD-1374: canonical human-operation context, set by
    /// [`Pm::with_activity_receipt`]. When set, each commit carries an
    /// `Activity-Receipt:` trailer. `None` for every other handle.
    activity_receipt: Option<serde_json::Value>,
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
            activity_receipt: None,
        })
    }

    /// CAD-538: attach the daemon's lease — `lock`/`try_lock`/`commit`
    /// then refuse once the lease fence trips and `commit` stamps the
    /// epoch. `pub(crate)`: only daemon code attaches; the CLI stays
    /// lease-free.
    pub(crate) fn attach_lease(&mut self, lease: crate::lease::PmLease) {
        self.lease = Some(lease);
    }

    /// CAD-1374: attach the server-prepared human context for this handle's
    /// commits. Only the allowlisted canonical fields are stored; a malformed
    /// context is refused here, before any write.
    pub(crate) fn with_activity_receipt(mut self, receipt: &serde_json::Value) -> Result<Self> {
        self.activity_receipt = Some(canonical_receipt(receipt)?);
        Ok(self)
    }

    /// CAD-1374: the `Activity-Receipt:` trailer for the staged admitted
    /// paths. The digest covers the exact staged diff of `rel`, taken before
    /// the commit; an oversized diff or any serialization failure is an error
    /// so the commit never happens unrecorded.
    fn receipt_trailer(&self, rel: &[String]) -> Result<Option<String>> {
        let Some(context) = &self.activity_receipt else {
            return Ok(None);
        };
        let digest = self.staged_digest(rel)?;
        let mut receipt = context.clone();
        let fields = receipt
            .as_object_mut()
            .ok_or_else(|| Error::internal("activity receipt is malformed"))?;
        fields.insert("event_id".into(), uuid::Uuid::new_v4().to_string().into());
        fields.insert("at".into(), time::now_epoch().into());
        fields.insert("outcome".into(), "committed".into());
        fields.insert("digest".into(), digest.into());
        let line = serde_json::to_string(&receipt)
            .map_err(|e| Error::internal(format!("activity receipt: {e}")))?;
        Ok(Some(format!("Activity-Receipt: {line}")))
    }

    /// CAD-1374: `sha256:<hex>` over the staged admitted diff, streamed in
    /// fixed chunks so memory is bounded by the buffer, not the diff. Stderr
    /// is discarded and never echoed. A read or exit failure kills and reaps
    /// only this child and refuses the commit.
    fn staged_digest(&self, rel: &[String]) -> Result<String> {
        use std::io::{ErrorKind, Read};
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.dir)
            .args(["diff", "--cached", "--binary", "--"])
            .args(rel)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = crate::reaper::spawn(&mut cmd)
            .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))?;
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::internal("activity receipt: no staged diff stream"));
        };
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let read = loop {
            match stdout.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => hasher.update(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
        drop(stdout);
        if read.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::internal(
                "activity receipt: staged diff could not be read",
            ));
        }
        let status = child
            .wait()
            .map_err(|_| Error::internal("activity receipt: git diff did not exit"))?;
        if !status.success() {
            return Err(Error::internal("activity receipt: staged diff failed"));
        }
        Ok(format!("sha256:{:x}", hasher.finalize()))
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
            std::fs::write(&gitignore, ".write.lock\n.index/\n.cadence/\n")?;
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
    /// allocated under this same lock. A kernel `flock` on a stable file
    /// (see [`pmlock`]); bounded wait. The lock dies with its process.
    #[track_caller]
    pub fn lock(&self) -> Result<PmLock> {
        self.acquire_default()
    }

    /// [`Self::lock`] with a caller-chosen wait.
    #[track_caller]
    pub fn lock_for(&self, wait: Duration) -> Result<PmLock> {
        self.acquire_for(wait)
    }

    /// [`Self::lock`] without the wait: `None` when another writer holds
    /// it. For a caller that must not stall behind a writer (the daemon
    /// under its own lock, CAD-449) and retries later instead.
    #[track_caller]
    pub fn try_lock(&self) -> Result<Option<PmLock>> {
        self.acquire(None, None, false)
    }

    /// [`Self::lock`] scoped to `paths`: when a crashed writer left
    /// the tracker mid-write, a write whose paths are disjoint from
    /// the leftovers is admitted; an overlapping write is refused, and
    /// git-level markers (`index.lock`, merge/rebase heads) refuse
    /// every write. The returned guard's commit must stay inside the
    /// declared paths — use [`Self::commit_scoped`], which enforces
    /// it. `paths` follow [`Self::commit`]'s convention (absolute or
    /// `pm.dir`-relative; outside the tracker is a caller bug).
    #[track_caller]
    pub fn lock_for_paths(&self, paths: &[PathBuf]) -> Result<PmLock> {
        self.acquire_scoped_default(paths)
    }

    /// [`Self::lock_for_paths`] without the wait: `None` when another
    /// writer holds it.
    #[track_caller]
    pub fn try_lock_for_paths(&self, paths: &[PathBuf]) -> Result<Option<PmLock>> {
        self.acquire(None, Some(paths), false)
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
    /// `paths` (absolute, or `pm.dir`-relative) as repo-relative
    /// strings, sorted and deduped. Shared by [`Self::commit`] and the
    /// scoped lock admission: one derivation, one caller-bug rule.
    ///
    /// A `..` component is a caller bug and refused: git resolves
    /// `dir/../../other` to `other` at commit time, which would let a
    /// scoped write pass the overlap check as one path and commit
    /// another. Git pathspec syntax is refused for the same reason —
    /// git would expand the pattern beyond the literal string the
    /// scope checked. `.` components and duplicate separators are normalized
    /// away by reconstruction; the tracker root itself (`pm.dir`,
    /// `.`) becomes the empty string, which the scope checks read as
    /// the whole tracker.
    fn rel_paths(&self, paths: &[PathBuf]) -> Result<Vec<String>> {
        use std::path::Component;
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
            let mut norm = PathBuf::new();
            for c in r.components() {
                match c {
                    Component::ParentDir => {
                        return Err(Error::internal(format!(
                            "tracker write {} escapes the PM dir (.. is refused)",
                            p.display()
                        )));
                    }
                    Component::CurDir => {}
                    _ => norm.push(c.as_os_str()),
                }
            }
            let rel_str = norm.to_string_lossy().into_owned();
            if pmlock::has_pathspec_magic(&rel_str) {
                return Err(Error::internal(format!(
                    "tracker write {} uses git pathspec syntax (refused: \
                     scope checks are literal)",
                    p.display()
                )));
            }
            rel.push(rel_str);
        }
        rel.sort();
        rel.dedup();
        Ok(rel)
    }

    /// [`Self::commit`] through a scoped [`PmLock`]: the commit's paths
    /// must stay inside the lock's admitted paths and clear of the
    /// crashed writer's leftovers, or the commit is refused and nothing
    /// is staged. A `Full` (unscoped) guard commits exactly like
    /// [`Self::commit`]. Callers that took [`Self::lock_for_paths`]
    /// must commit through here, never through [`Self::commit`].
    pub fn commit_scoped(
        &self,
        lock: &PmLock,
        paths: &[PathBuf],
        message: &str,
    ) -> Result<Vec<String>> {
        self.fence_check()?;
        lock.check_commit(&self.rel_paths(paths)?)?;
        self.commit(paths, message)
    }

    pub fn commit(&self, paths: &[PathBuf], message: &str) -> Result<Vec<String>> {
        self.fence_check()?;
        let rel = self.rel_paths(paths)?;
        if rel.is_empty() {
            return Err(Error::internal(
                "a tracker commit must name the paths it wrote",
            ));
        }
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
        // CAD-1374: the human receipt rides the same commit as the
        // business change; its digest covers the staged admitted paths.
        let message = match self.receipt_trailer(&rel) {
            Ok(Some(trailer)) => {
                let body = message.trim_end_matches('\n');
                let joined = body.rsplit('\n').next().is_some_and(|l| l.contains(": "));
                format!("{body}{}{trailer}\n", if joined { "\n" } else { "\n\n" })
            }
            Ok(None) => message,
            Err(e) => {
                unstage(&self.dir, &rel);
                return Err(e);
            }
        };
        // CAD-538: the lease epoch rides every commit a leased daemon
        // makes — a reader can order writes across restarts and
        // holdovers. The trailer joins the closing block directly, or
        // starts one after a blank line when the message ends bare.
        let message = match self.lease.as_ref().and_then(|lease| lease.epoch()) {
            Some(epoch) => {
                let body = message.trim_end_matches('\n');
                let trailer_shaped = body.rsplit('\n').next().is_some_and(|l| l.contains(": "));
                format!(
                    "{body}{}Lease-Epoch: {}\n",
                    if trailer_shaped { "\n" } else { "\n\n" },
                    epoch
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

    /// The lease epoch this PM's writes carry, when it is leased. A
    /// recovery commit that cannot go through [`Pm::commit`] still
    /// names it, so a reader can order its commit against the others
    /// across a restart or a holdover.
    pub fn lease_epoch(&self) -> Option<String> {
        self.lease
            .as_ref()
            .and_then(|lease| lease.epoch())
            .map(|epoch| epoch.to_string())
    }

    /// CAD-538 — the tracker half of the SIGTERM flush: commit whatever
    /// the index already stages, the pending write a crash or kill left
    /// mid-flight. Never `git add` — worktree dirt and unstaged edits
    /// are not this flush's to claim, and a planted file somebody only
    /// dropped must not ride in. `Ok(false)` when nothing was staged or
    /// the tracker lock was busy. A leased daemon that has lost its
    /// lease is refused here like every other write.
    pub fn flush_pending(&self, actor: &str) -> Result<bool> {
        self.flush_pending_cancellable(actor, &FlushCancel::default())
    }

    /// [`Self::flush_pending`] whose `git commit` is owned by `cancel`:
    /// once cancelled, a commit not yet started is refused and one in
    /// flight is terminated and reaped before this returns — no flush
    /// writer outlives the cancel (CAD-694). Only the commit writes; the
    /// read-only `git diff` probe needs no ownership.
    pub fn flush_pending_cancellable(&self, actor: &str, cancel: &FlushCancel) -> Result<bool> {
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
        if let Some(epoch) = self.lease.as_ref().and_then(|lease| lease.epoch()) {
            message.push_str(&format!("Lease-Epoch: {epoch}\n"));
        }
        git_cancellable(
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
            cancel,
            self.lease.as_ref(),
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
        let foreign = |p: &str| {
            !ours.contains(p)
                && p != ".write.lock"
                && !p.starts_with(".index/")
                && !p.starts_with(".cadence/chat-files/")
        };
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
/// signal for the operator watching the CLI or the daemon's log. A
/// path already named within `FOREIGN_WARN_QUIET` folds into the
/// "(already reported)" count instead of printing again — one stuck
/// file must not train operators to skip every warning (CAD-759).
fn warn_foreign(dir: &Path, foreign: &[String], extra: usize) {
    if foreign.is_empty() && extra == 0 {
        return;
    }
    let fresh = foreign_fresh(dir, foreign, time::now_epoch());
    let suppressed = foreign.len() - fresh.len();
    if fresh.is_empty() && extra == 0 {
        return;
    }
    let quiet = if suppressed > 0 {
        format!(" ({suppressed} already reported within 24h)")
    } else {
        String::new()
    };
    eprintln!(
        "warning: {} foreign path(s) under {} left uncommitted: {}{}",
        fresh.len() + extra,
        dir.display(),
        foreign_listed(&fresh, extra),
        quiet
    );
}

/// How long a warned-about foreign path stays quiet — the CAD-584
/// artifact warned on every write for days; one notice per path per
/// day keeps the signal readable.
const FOREIGN_WARN_QUIET: i64 = 24 * 3600;

/// `.index/foreign-seen` — `path<TAB>epoch` for each foreign path the
/// warning already named. Gitignored tracker scratch like `.write.lock`.
fn load_foreign_seen(dir: &Path) -> HashMap<String, i64> {
    std::fs::read_to_string(dir.join(".index").join("foreign-seen"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (p, t) = l.rsplit_once('\t')?;
            Some((p.to_string(), t.trim().parse().ok()?))
        })
        .collect()
}

/// Foreign paths not warned about within the quiet window — and stamps
/// *those* paths seen at `now`. A suppressed path keeps its last-warned
/// stamp, so a file that stays dirty re-warns a day after the last
/// warning instead of sliding quiet forever. Best effort both ways: a
/// state file that cannot be read or written degrades to
/// warn-everything, never to silence.
fn foreign_fresh(dir: &Path, foreign: &[String], now: i64) -> Vec<String> {
    let mut seen = load_foreign_seen(dir);
    let fresh: Vec<String> = foreign
        .iter()
        .filter(|p| seen.get(*p).is_none_or(|t| now - *t >= FOREIGN_WARN_QUIET))
        .cloned()
        .collect();
    for p in &fresh {
        seen.insert(p.clone(), now);
    }
    // Anything older than two quiet windows cannot decide a verdict
    // again — the file stays bounded no matter the history.
    seen.retain(|_, t| now - *t < 2 * FOREIGN_WARN_QUIET);
    let body: String = seen.iter().map(|(p, t)| format!("{p}\t{t}\n")).collect();
    let index = dir.join(".index");
    let _ = std::fs::create_dir_all(&index)
        .and_then(|_| std::fs::write(index.join("foreign-seen"), body));
    fresh
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

/// Ownership handle for a shutdown flush's writer (CAD-694). The flush
/// runs on a worker the shutdown tail stops waiting for when its budget
/// lapses; cancelling makes that worker's `git commit` die with it, so
/// the lease can transfer by TTL without a late commit racing the
/// successor's epoch.
#[derive(Default)]
pub struct FlushCancel(std::sync::atomic::AtomicBool);

impl FlushCancel {
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// How long a cancelled commit gets to unwind (git removes its lock
/// files on SIGTERM) before the whole group is killed outright.
const CANCEL_TERM_GRACE: Duration = Duration::from_millis(1000);

/// Whether the leader of `group` has exited, WITHOUT reaping it — the
/// zombie keeps the pgid ours, so signalling the group stays safe.
fn leader_exited(group: i32) -> bool {
    // SAFETY: waitid with WNOWAIT|WNOHANG only inspects a child of ours.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let rc = libc::waitid(
            libc::P_PID,
            group as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        );
        rc != 0 || info.si_pid() != 0
    }
}

/// The commit's own process group is out of reach of group signals sent
/// to the daemon, so a SIGKILLed daemon would leave it running to land a
/// late `cadence flush on stop` carrying an old lease epoch. Ask the
/// kernel to kill it with its parent (Linux), and exit at once if the
/// parent already died before the request took effect.
///
/// Residual: `PR_SET_PDEATHSIG` fires when the spawning THREAD exits,
/// which here is the flush worker — alive for the whole commit. On other
/// platforms (macOS) there is no equivalent and a crash can orphan the
/// commit; the successor's lease epoch still orders it, but the commit
/// itself is not prevented.
#[cfg(target_os = "linux")]
fn die_with_parent(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: only async-signal-safe calls (prctl, getppid, _exit) run
    // between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn die_with_parent(_cmd: &mut Command) {}

/// [`git`] that the owning thread polls against `cancel` AND the lease:
/// a lost lease or one with less than the lease's [`commit_floor`](crate::lease::PmLease::commit_floor) of validity
/// left cancels the commit like an explicit cancel. The child runs in
/// its own process group; the owner terminates the group, then kills it
/// outright after the grace (a hook that traps TERM included) with the
/// leader still unreaped, and never joins the pipe drains on that path —
/// so the worker unwinds, drops the tracker lock, and no writer is left.
fn git_cancellable(
    dir: &Path,
    args: &[&str],
    cancel: &FlushCancel,
    lease: Option<&crate::lease::PmLease>,
) -> Result<String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let must_stop = || {
        cancel.cancelled()
            || lease.is_some_and(|l| l.remaining().is_some_and(|r| r < l.commit_floor()))
    };
    if must_stop() {
        return Err(Error::rejected(
            "tracker flush cancelled before its commit (cancelled, or the lease is too close to lapsing)",
        ));
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    die_with_parent(&mut cmd);
    let mut child = crate::reaper::spawn(&mut cmd)
        .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let group = child.id() as i32;
    let status = loop {
        if must_stop() {
            // SAFETY: the group's leader is our child and stays
            // unreaped until after the last signal, so the id is ours.
            unsafe { libc::kill(-group, libc::SIGTERM) };
            // The TERM grace plus the kill must fit in the lease's own
            // floor: half of it, at most the default grace.
            let grace = lease.map_or(CANCEL_TERM_GRACE, |l| {
                CANCEL_TERM_GRACE.min(l.commit_floor() / 2)
            });
            let deadline = std::time::Instant::now() + grace;
            while std::time::Instant::now() < deadline && !leader_exited(group) {
                std::thread::sleep(Duration::from_millis(10));
            }
            // SAFETY: as above; hooks that ignored TERM die here.
            unsafe { libc::kill(-group, libc::SIGKILL) };
            let _ = child.wait();
            // The drain threads end when the group's pipes close; they
            // are not joined — a straggler must not hold the worker.
            return Err(Error::rejected(
                "tracker flush cancelled — its commit was terminated",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                // SAFETY: still our unreaped child's group.
                unsafe { libc::kill(-group, libc::SIGKILL) };
                let _ = child.wait();
                return Err(Error::internal(format!("waiting on git: {e}")));
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        return Err(Error::rejected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
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

/// CAD-1374: the human-operation actions a receipt may carry (the agreed
/// 6 issue + 5 wiki canonical names).
const RECEIPT_ACTIONS: [&str; 11] = [
    "issue_created",
    "issue_updated",
    "issue_link_changed",
    "issue_ref_added",
    "issue_comment_added",
    "issue_artifact_added",
    "wiki_written",
    "wiki_blob_uploaded",
    "wiki_directory_created",
    "wiki_moved",
    "wiki_removed",
];
const RECEIPT_TEXT_MAX: usize = 256;
/// Wiki paths allow 4160 bytes at the producer; moves carry two of them.
const RECEIPT_RESOURCE_MAX: usize = 8192;

/// Select the canonical fields of a server-prepared human context. Any
/// malformed or unlisted value is refused; no raw payload is kept.
fn canonical_receipt(raw: &serde_json::Value) -> Result<serde_json::Value> {
    let malformed = || Error::rejected("activity receipt context is malformed");
    let obj = raw.as_object().ok_or_else(malformed)?;
    if obj
        .get("schema_version")
        .and_then(serde_json::Value::as_i64)
        != Some(1)
    {
        return Err(malformed());
    }
    let text = |v: Option<&serde_json::Value>| -> Result<String> {
        let s = v
            .and_then(serde_json::Value::as_str)
            .ok_or_else(malformed)?;
        if s.is_empty() || s.len() > RECEIPT_TEXT_MAX || s.chars().any(char::is_control) {
            return Err(malformed());
        }
        Ok(s.to_string())
    };
    let uuid_field = |v: Option<&serde_json::Value>| -> Result<String> {
        let s = text(v)?;
        uuid::Uuid::parse_str(&s)
            .map(|u| u.to_string())
            .map_err(|_| malformed())
    };
    // causation_id is not inferred from the browser: it must be present and
    // null (no causing event), and it is emitted as null.
    if obj.get("causation_id") != Some(&serde_json::Value::Null) {
        return Err(malformed());
    }
    let company = text(obj.get("company"))?;
    let human = obj
        .get("human")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(malformed)?;
    let role = text(human.get("role"))?;
    if text(human.get("company"))? != company || !matches!(role.as_str(), "operator" | "member") {
        return Err(malformed());
    }
    let resource = obj
        .get("resource")
        .and_then(serde_json::Value::as_str)
        .filter(|s| {
            !s.is_empty() && s.len() <= RECEIPT_RESOURCE_MAX && !s.chars().any(char::is_control)
        })
        .ok_or_else(malformed)?
        .to_string();
    let action = text(obj.get("action"))?;
    if !RECEIPT_ACTIONS.contains(&action.as_str()) {
        return Err(malformed());
    }
    let mut human_out = serde_json::json!({
        "company": company,
        "sub": text(human.get("sub"))?,
        "session": text(human.get("session"))?,
        "role": role,
    });
    // Optional display name: kept only when safe, otherwise omitted.
    if let Some(display) = human.get("display").and_then(serde_json::Value::as_str) {
        if !display.is_empty()
            && display.len() <= RECEIPT_TEXT_MAX
            && !display.chars().any(char::is_control)
        {
            human_out["display"] = display.into();
        }
    }
    Ok(serde_json::json!({
        "schema_version": 1,
        "company": company,
        "human": human_out,
        "action": action,
        "resource": resource,
        "reason": text(obj.get("reason"))?,
        "request_id": uuid_field(obj.get("request_id"))?,
        "correlation_id": uuid_field(obj.get("correlation_id"))?,
        "causation_id": serde_json::Value::Null,
    }))
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
- Every write is one git commit, serialised by a kernel lock on\n\
  `.git/cadence-write.flock` (`.write.lock` fences older binaries).\n";

#[cfg(test)]
mod lock_tests;

#[cfg(test)]
#[path = "cad1177_tools_acceptance.rs"]
mod cad1177_tools_acceptance;

#[cfg(test)]
mod cad1298_acceptance;

#[cfg(test)]
mod cad1298_readiness_acceptance;

#[cfg(test)]
mod cad1314_acceptance;

#[cfg(test)]
mod cad1793_acceptance;

#[cfg(test)]
#[path = "cad1270_screen_decl_acceptance.rs"]
mod cad1270_screen_decl_acceptance;

#[cfg(all(test, feature = "test-seam"))]
pub(crate) fn set_lock_waiting_observer_for_test(sender: Option<std::sync::mpsc::Sender<()>>) {
    pmlock::WAITING.with(|observer| *observer.borrow_mut() = sender);
}
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

    /// CAD-759: a foreign path warns once per quiet window — repeated
    /// writes against the same stuck file say so instead of repeating
    /// the whole list, and a second stuck path still names itself.
    #[test]
    fn foreign_warning_dedupes_per_path_per_day() {
        let dir = tempfile::TempDir::new().unwrap();
        let pm_dir = dir.path().join("pm");
        let _pm = Pm::init(&pm_dir).unwrap();
        let f = vec!["cadence/CAD-1/artifacts/review.md".to_string()];
        assert_eq!(foreign_fresh(&pm_dir, &f, 1_000_000), f);
        // Inside the window the path stays quiet while a second one
        // still names itself.
        let both = vec![f[0].clone(), "other/x.md".to_string()];
        assert_eq!(foreign_fresh(&pm_dir, &both, 1_000_100), ["other/x.md"]);
        // Past the window the stuck path warns again.
        assert_eq!(
            foreign_fresh(&pm_dir, &f, 1_000_000 + FOREIGN_WARN_QUIET),
            f
        );
        // The state file lives under gitignored .index/ — the foreign
        // scan itself can never warn on the dedup record.
        let seen = std::fs::read_to_string(pm_dir.join(".index/foreign-seen")).unwrap();
        assert!(seen.contains("cadence/CAD-1/artifacts/review.md"), "{seen}");
    }
}
