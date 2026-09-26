//! `cadence update` (CAD-561) — the everyday path: one command that
//! wraps lease, backup, attested install, bounded drain, restart,
//! health check and auto-rollback.
//!
//! `upgrade` and `rollout` stay the low-level layer; this module
//! orchestrates them:
//!
//! 1. **check** — newest green `main` build ([`crate::upgrade`]), the
//!    merged PR titles between it and the linked release, whether the
//!    new build's schema crosses the store's, and what would block
//!    (a lease another identity holds, in-flight turns);
//! 2. **lease** — auto-claimed for the target and auto-released at the
//!    end. A lease another identity holds is reported as "update in
//!    progress by X since T", never as a raw lease error;
//! 3. **backup** — `cadence backup` into a directory *outside* the
//!    state dir by default, recorded as the lease's backup receipt, so
//!    a schema crossing is authorized and a restore has a manifest;
//! 4. **install** — the attested CI build, side by side under
//!    `<releases>/<sha>` (the link is not moved yet);
//! 5. **drain** — the daemon stops starting new turns
//!    ([`crate::daemon`]'s `update_drain`), the command shows exactly
//!    what it waits on ("waiting for 3 turns: swe-554 (12m), …") and
//!    waits at most `--drain` (default 10m). `--now` switches at once;
//! 6. **switch** — the new binary's `daemon restart --ui` (running
//!    turns are interrupted only when the drain timed out; the
//!    existing resume/adoption path brings them back);
//! 7. **health** — the daemon and the board must answer health on the
//!    new build within [`HEALTH_TIMEOUT`]; otherwise the link is
//!    repointed back to the previous release, restarted, and the
//!    failure is reported;
//! 8. **commit** — the previous [`DEFAULT_KEEP`] releases are kept,
//!    older ones pruned; the drain is lifted and the lease released.
//!
//! Operator-only throughout: the CLI path is refused inside a pane and
//! outside one it needs `--as <identity>` with the same process proof
//! as `rollout claim --as`; the board path is the operator's session
//! plus peer proof. Agents can never trigger an update.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::rollout;
use crate::upgrade::{self, Current, Layout, ReleaseSource};

/// How long the drain waits for in-flight turns before switching anyway.
pub const DEFAULT_DRAIN: Duration = Duration::from_secs(600);
/// How many previous releases stay under the releases dir.
pub const DEFAULT_KEEP: usize = 3;
/// The daemon (and board) must answer health on the new build within this.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(90);
/// The lease an update takes; long enough for a slow drain and a restart.
pub const LEASE_TTL: Duration = Duration::from_secs(3600);
/// The `reason` of the pre-update backup and the lease's claim.
pub const BACKUP_REASON: &str = "pre-update";
/// Backups kept in the update backup directory (the same `--reason`).
pub const BACKUP_KEEP: usize = 5;
/// The state-dir file naming a pending update, read by the daemon at
/// boot (so a restart mid-update stays drained) and by the board (the
/// draining banner). Written and removed by the update itself.
pub const UPDATE_FILE: &str = "update.json";
/// A pending-update file older than this is stale — an update that
/// died without cleaning up — and the daemon ignores it.
pub const UPDATE_STALE_SECS: f64 = 2.0 * 3600.0;

/// The default backup directory for an update: a sibling of the state
/// dir, so the copy survives a state-dir mishap and needs no manual
/// copy for `rollout backup` (CAD-561 acceptance 3).
pub fn default_backup_dir(state_dir: &Path) -> PathBuf {
    state_dir
        .parent()
        .unwrap_or(state_dir)
        .join("cadence-backups")
}

/// `<state dir>/update.json`.
pub fn update_file(state_dir: &Path) -> PathBuf {
    state_dir.join(UPDATE_FILE)
}

/// What a pending update looks like to another process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingUpdate {
    /// `draining`, `switching`, `verifying` — the phase the update is in.
    pub phase: String,
    pub target: String,
    pub from: Option<String>,
    pub by: String,
    pub since: f64,
}

impl PendingUpdate {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Read `<state>/update.json` — `None` when absent, unreadable, or
/// older than [`UPDATE_STALE_SECS`].
pub fn pending_update(state_dir: &Path) -> Option<PendingUpdate> {
    let text = std::fs::read_to_string(update_file(state_dir)).ok()?;
    let pending: PendingUpdate = serde_json::from_str(&text).ok()?;
    let now = rollout::unix_now();
    (pending.since > 0.0 && now - pending.since < UPDATE_STALE_SECS).then_some(pending)
}

/// Write the pending-update marker (the daemon and the board read it).
pub fn write_pending(state_dir: &Path, pending: &PendingUpdate) -> Result<()> {
    let path = update_file(state_dir);
    let text = serde_json::to_string_pretty(pending)
        .map_err(|e| Error::internal(format!("update.json: {e}")))?;
    std::fs::write(&path, text)?;
    Ok(())
}

/// Remove the marker. Missing is fine.
pub fn clear_pending(state_dir: &Path) {
    let _ = std::fs::remove_file(update_file(state_dir));
}

/// One in-flight turn — what the drain waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiter {
    pub alias: String,
    pub message: String,
    pub state: String,
    pub age_secs: u64,
}

impl Waiter {
    /// `swe-554 (12m)` — minutes once past a minute, else seconds.
    pub fn render(&self) -> String {
        if self.age_secs >= 60 {
            format!("{} ({}m)", self.alias, self.age_secs / 60)
        } else {
            format!("{} ({}s)", self.alias, self.age_secs)
        }
    }
}

/// `waiting for 3 turns: swe-554 (12m), …`
pub fn render_waiting(waiters: &[Waiter]) -> String {
    let list = waiters
        .iter()
        .map(Waiter::render)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "waiting for {} turn{}: {list}",
        waiters.len(),
        if waiters.len() == 1 { "" } else { "s" }
    )
}

/// What `cadence update --check` reports, changing nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub current: Option<String>,
    pub current_version: Option<String>,
    pub target: String,
    pub target_version: Option<String>,
    pub up_to_date: bool,
    /// Merged PR titles between the linked release and the target.
    pub changes: Vec<String>,
    /// The store's schema, when a database exists.
    pub schema_current: Option<i64>,
    /// The target build's `SCHEMA_VERSION`, read from the repository at
    /// that sha — `None` when it cannot be read.
    pub schema_target: Option<i64>,
    /// The target build's schema is newer than the store's.
    pub migration: bool,
    /// The active lease, when one is held.
    pub lease: Option<Value>,
    /// In-flight turns right now.
    pub waiters: Vec<Waiter>,
    /// What would block an update, in operator words.
    pub blockers: Vec<String>,
}

impl CheckReport {
    /// The plain progress lines (the `--json` form keeps the struct).
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        match (&self.current, &self.current_version) {
            (Some(sha), Some(version)) => out.push(format!("current: {sha} ({version})")),
            (Some(sha), None) => out.push(format!("current: {sha}")),
            (None, _) => out.push("current: no cadence link installed".to_string()),
        }
        let mut target = format!("available: {}", self.target);
        if let Some(version) = &self.target_version {
            target.push_str(&format!(" ({version})"));
        }
        if self.up_to_date {
            out.push(format!("{target} — already installed"));
        } else {
            out.push(target);
        }
        if !self.changes.is_empty() {
            out.push(format!("changes: {} merged PR(s)", self.changes.len()));
            for change in &self.changes {
                out.push(format!("  - {change}"));
            }
        } else if !self.up_to_date {
            out.push("changes: none recorded between the two builds".to_string());
        }
        out.push(match (self.schema_current, self.schema_target) {
            (Some(from), Some(to)) if to > from => format!(
                "schema: migration {from} → {to} (covered by the pre-update backup receipt)"
            ),
            (Some(from), Some(to)) if to == from => format!("schema: no change ({from})"),
            (Some(from), Some(to)) => format!("schema: target {to} is older than the store {from}"),
            (Some(from), None) => {
                format!("schema: store {from}; the target build's schema could not be read")
            }
            (None, Some(to)) => format!("schema: fresh store; target build is {to}"),
            (None, None) => "schema: no database yet".to_string(),
        });
        if let Some(lease) = &self.lease {
            out.push(format!(
                "lease: held by {} since {}",
                lease["holder"].as_str().unwrap_or("?"),
                fmt_epoch(lease["claimed_at"].as_f64().unwrap_or(0.0))
            ));
        } else {
            out.push("lease: free".to_string());
        }
        if self.waiters.is_empty() {
            out.push("busy: no turns in flight".to_string());
        } else {
            out.push(format!("busy: {}", render_waiting(&self.waiters)));
        }
        for blocker in &self.blockers {
            out.push(format!("blocked: {blocker}"));
        }
        out
    }

    pub fn to_json(&self) -> Value {
        json!({
            "current": self.current,
            "current_version": self.current_version,
            "target": self.target,
            "target_version": self.target_version,
            "up_to_date": self.up_to_date,
            "changes": self.changes,
            "change_count": self.changes.len(),
            "schema": {"current": self.schema_current, "target": self.schema_target,
                       "migration": self.migration},
            "lease": self.lease,
            "waiters": self.waiters.iter().map(|w| json!({
                "alias": w.alias, "message": w.message, "state": w.state,
                "age_secs": w.age_secs,
            })).collect::<Vec<_>>(),
            "blockers": self.blockers,
        })
    }
}

/// `cadence update`'s options.
#[derive(Debug, Clone)]
pub struct Options {
    /// Wait at most this long for in-flight turns before switching.
    pub drain: Duration,
    /// Switch immediately: no drain wait (interrupted turns resume).
    pub now: bool,
    /// Previous releases kept under the releases dir.
    pub keep: usize,
    /// Where the pre-update backup goes [default: [`default_backup_dir`]].
    pub backup_dir: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            drain: DEFAULT_DRAIN,
            now: false,
            keep: DEFAULT_KEEP,
            backup_dir: None,
        }
    }
}

/// The host the pipeline runs against: production talks to the daemon
/// and the filesystem; tests pass a fake. Every step the acceptance
/// names is behind this seam.
pub trait UpdateHost {
    fn state_dir(&self) -> &Path;
    fn layout(&self) -> &Layout;
    fn source(&self) -> &dyn ReleaseSource;
    /// The proven operator identity the lease is claimed under.
    fn identity(&self) -> &str;
    /// One plain progress line.
    fn progress(&self, line: &str);
    /// In-flight turns right now (the drain's waiters).
    fn waiters(&self) -> Result<Vec<Waiter>>;
    /// Record the pending-update phase, or clear it with `None`.
    fn set_pending(&self, pending: Option<&PendingUpdate>) -> Result<()>;
    /// Ask the daemon to stop starting new turns (`true`) or lift it.
    fn set_drain(&self, on: bool) -> Result<()>;
    /// Restart the daemon on `binary` (the new release), blocking until
    /// the restart command returns.
    fn restart(&self, binary: &Path) -> Result<()>;
    /// The answering daemon's build commit (`None`: no daemon).
    fn daemon_build(&self) -> Result<Option<String>>;
    /// The answering board's build commit (`None`: no board running).
    fn board_build(&self) -> Result<Option<String>>;
    fn now(&self) -> f64;
    fn sleep(&self, duration: Duration);
}

/// What `cadence update` did, for `--json` and the report.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub check: CheckReport,
    pub lease: Option<Value>,
    pub backup: Option<Value>,
    pub install: Option<Value>,
    pub drain: Value,
    pub restart: Value,
    pub health: Value,
    pub rolled_back: bool,
    pub pruned: Vec<String>,
    pub lines: Vec<String>,
}

impl RunReport {
    pub fn to_json(&self) -> Value {
        json!({
            "check": self.check.to_json(),
            "lease": self.lease,
            "backup": self.backup,
            "install": self.install,
            "drain": self.drain,
            "restart": self.restart,
            "health": self.health,
            "rolled_back": self.rolled_back,
            "pruned": self.pruned,
            "lines": self.lines,
        })
    }
}

fn fmt_epoch(secs: f64) -> String {
    if secs <= 0.0 {
        return "unknown".to_string();
    }
    crate::issue::time::basic(secs as i64)
}

/// The linked release's version string: the running process's own when
/// the link points at it, else reconstructed from a verified CI
/// manifest, else `None` (the caller prints the sha alone).
fn version_of(layout: &Layout, sha: &str) -> Option<String> {
    let binary = layout.binary(sha);
    if let (Ok(me), Ok(target)) = (std::env::current_exe(), std::fs::canonicalize(&binary)) {
        if std::fs::canonicalize(&me).is_ok_and(|me| me == target) {
            return Some(format!(
                "{}+{}",
                env!("CARGO_PKG_VERSION"),
                crate::overview::BUILD_COMMIT
            ));
        }
    }
    let manifest = layout.release_dir(sha).join(upgrade::MANIFEST);
    let text = std::fs::read_to_string(manifest).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    (manifest["source_sha"].as_str() == Some(sha))
        .then(|| format!("{}+{sha}", env!("CARGO_PKG_VERSION")))
}

/// `cadence update --check`: resolve the newest green main build, the
/// linked release, the changes between them, the schema crossing and
/// the blockers — changing nothing.
pub fn check(host: &dyn UpdateHost) -> Result<CheckReport> {
    let layout = host.layout();
    let src = host.source();
    let state_dir = host.state_dir();
    let current = upgrade::current(layout)?;
    if current == Current::NotSymlink {
        return Err(Error::rejected(format!(
            "{} exists and is not a symlink — refusing to replace it. Move it aside \
             (or into {}/<sha>/cadence), then rerun",
            layout.link.display(),
            layout.releases.display()
        )));
    }
    let current_sha = match &current {
        Current::Link { sha, .. } => sha.clone(),
        _ => None,
    };
    src.check_auth()?;
    let run = src.latest_green_main()?.ok_or_else(|| {
        Error::rejected(format!(
            "no successful `{}` push run on {} in {} — nothing to install; check \
             `gh run list --workflow {} --branch {}`",
            upgrade::WORKFLOW,
            upgrade::MAIN,
            src.repo(),
            upgrade::WORKFLOW,
            upgrade::MAIN
        ))
    })?;
    let target = run.head_sha.clone();
    if !upgrade::is_full_sha(&target) {
        return Err(Error::internal(format!(
            "run {} reports head sha `{target}`, not a full commit id",
            run.id
        )));
    }
    let up_to_date = current_sha.as_deref() == Some(target.as_str());
    // A target that is not on main can never be installed; refuse here,
    // before the lease and the backup, not deep inside the install.
    if !up_to_date {
        match src.on_main(&target)? {
            upgrade::OnMain::Yes => {}
            upgrade::OnMain::No(status) => {
                return Err(Error::rejected(format!(
                    "{target} is not on {} in {} (compare status: {status}) — only builds of \
                     merged commits are installable",
                    upgrade::MAIN,
                    src.repo()
                )))
            }
            upgrade::OnMain::Unknown => {
                return Err(Error::rejected(format!(
                    "{target} is not a commit {} knows — not on {}",
                    src.repo(),
                    upgrade::MAIN
                )))
            }
        }
    }
    let changes = match (&current_sha, up_to_date) {
        (Some(from), false) => src.merged_titles(from, &target).unwrap_or_default(),
        _ => Vec::new(),
    };
    let schema_current = rollout::store_schema(state_dir)?;
    let schema_target = if up_to_date {
        schema_current
    } else {
        src.schema_version(&target).unwrap_or(None)
    };
    let migration = matches!((schema_current, schema_target), (Some(from), Some(to)) if to > from);
    let lease = rollout::status(state_dir)?;
    let lease = (lease["held"].as_bool() == Some(true) && lease["expired"].as_bool() != Some(true))
        .then_some(lease);
    let waiters = host.waiters()?;
    let mut blockers = Vec::new();
    if let Some(lease) = &lease {
        let holder = lease["holder"].as_str().unwrap_or("?");
        if holder != host.identity() {
            blockers.push(format!(
                "update in progress by {holder} since {}",
                fmt_epoch(lease["claimed_at"].as_f64().unwrap_or(0.0))
            ));
        }
    }
    if !waiters.is_empty() {
        blockers.push(format!(
            "{} in flight (the drain waits up to {} before switching)",
            render_waiting(&waiters),
            fmt_duration(host, DEFAULT_DRAIN)
        ));
    }
    Ok(CheckReport {
        current: current_sha.clone(),
        current_version: current_sha.as_deref().and_then(|s| version_of(layout, s)),
        target: target.clone(),
        target_version: version_of(layout, &target),
        up_to_date,
        changes,
        schema_current,
        schema_target,
        migration,
        lease,
        waiters,
        blockers,
    })
}

fn fmt_duration(_host: &dyn UpdateHost, d: Duration) -> String {
    let secs = d.as_secs();
    if secs.is_multiple_of(60) && secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// The lease an update holds, and what it released.
struct Lease {
    caller: rollout::Caller,
    /// True when this run claimed it (so it releases it); false when an
    /// existing lease of the same identity was reused.
    claimed: bool,
}

/// Claim the lease for `target`, or reuse the identity's own live lease.
/// A lease another identity holds refuses with the operator-facing
/// sentence, never a raw lease error.
fn take_lease(host: &dyn UpdateHost, target: &str) -> Result<Lease> {
    let state_dir = host.state_dir();
    let caller = rollout::Caller {
        identity: host.identity().to_string(),
        source: "as",
    };
    let status = rollout::status(state_dir)?;
    if status["held"].as_bool() == Some(true) && status["expired"].as_bool() != Some(true) {
        let holder = status["holder"].as_str().unwrap_or("?");
        if holder != host.identity() {
            return Err(Error::rejected(format!(
                "update in progress by {holder} since {} — wait for it to finish, or \
                 `cadence rollout status` to see the lease",
                fmt_epoch(status["claimed_at"].as_f64().unwrap_or(0.0))
            )));
        }
        return Ok(Lease {
            caller,
            claimed: false,
        });
    }
    rollout::claim(
        state_dir,
        &rollout::ClaimRequest {
            caller: &caller,
            reason: "cadence update",
            target: Some(target),
            ttl: LEASE_TTL,
            takeover: false,
            now: host.now(),
        },
    )?;
    Ok(Lease {
        caller,
        claimed: true,
    })
}

fn release_lease(host: &dyn UpdateHost, lease: &Lease) -> Result<()> {
    if !lease.claimed {
        return Ok(());
    }
    rollout::release(host.state_dir(), &lease.caller).map(|_| ())
}

/// Take the pre-update backup (outside the state dir by default) and
/// record it as the lease's receipt — no manual copy step.
fn take_backup(host: &dyn UpdateHost, opts: &Options) -> Result<Value> {
    let state_dir = host.state_dir();
    let dir = opts
        .backup_dir
        .clone()
        .unwrap_or_else(|| default_backup_dir(state_dir));
    let backup =
        crate::backup::backup(state_dir, &dir, BACKUP_KEEP, BACKUP_REASON).map_err(|e| {
            Error::rejected(format!(
                "update refused before installing: the pre-update backup into {} failed: {e}",
                dir.display()
            ))
        })?;
    let db = backup["db"].as_str().unwrap_or_default().to_string();
    if db.is_empty() {
        return Err(Error::rejected(
            "update refused before installing: the pre-update backup reported no db path",
        ));
    }
    // CAD-314's lesson: the copy must still be on disk and verify when
    // the install starts. Retention in the backup dir runs before this.
    crate::backup::verify(Path::new(backup["manifest"].as_str().unwrap_or_default()))?;
    let receipt = rollout::record_backup(state_dir, &lease_caller(host), Path::new(&db))?;
    Ok(json!({"backup": backup, "receipt": receipt}))
}

/// The lease caller for `record_backup` (the identity the lease is under).
fn lease_caller(host: &dyn UpdateHost) -> rollout::Caller {
    rollout::Caller {
        identity: host.identity().to_string(),
        source: "as",
    }
}

/// Wait for the fleet to quiesce, bounded. `Ok(waiters_left)` — the
/// turns still running when the bound was reached (empty: quiet).
pub fn drain_wait(
    host: &dyn UpdateHost,
    deadline_secs: u64,
    on_tick: &mut dyn FnMut(&[Waiter]),
) -> Result<Vec<Waiter>> {
    let started = host.now();
    loop {
        let waiters = host.waiters()?;
        if waiters.is_empty() {
            return Ok(Vec::new());
        }
        on_tick(&waiters);
        if host.now() - started >= deadline_secs as f64 {
            return Ok(waiters);
        }
        host.sleep(Duration::from_secs(2));
    }
}

/// Poll `build` until it reports `want`, or the deadline passes.
pub fn health_wait(
    host: &dyn UpdateHost,
    want: &str,
    timeout: Duration,
    mut board: impl FnMut() -> Result<Option<String>>,
) -> Result<()> {
    let deadline = host.now() + timeout.as_secs_f64();
    loop {
        let daemon = host.daemon_build()?;
        let board_build = board()?;
        let daemon_ok = daemon.as_deref() == Some(want);
        let board_ok = board_build.is_none() || board_build.as_deref() == Some(want);
        if daemon_ok && board_ok {
            return Ok(());
        }
        if host.now() >= deadline {
            return Err(Error::rejected(format!(
                "health check failed after {}s: daemon answered {}, board answered {}; \
                 expected the new build {want}",
                timeout.as_secs(),
                daemon.as_deref().unwrap_or("nothing"),
                board_build.as_deref().unwrap_or("nothing")
            )));
        }
        host.sleep(Duration::from_secs(1));
    }
}

/// The switch: restart the daemon on the already-installed `sha`. The
/// install itself is [`upgrade::run`]'s (attested, hash-matched, side
/// by side — it also moves the link).
fn restart_on(host: &dyn UpdateHost, sha: &str) -> Result<Value> {
    let binary = host.layout().binary(sha);
    host.progress(&format!("switching: restarting the daemon on {sha}"));
    host.restart(&binary)?;
    Ok(json!({"binary": binary, "sha": sha}))
}

/// Run the update. `--check`/`status` never reach here.
pub fn run(host: &dyn UpdateHost, opts: &Options) -> Result<RunReport> {
    let state_dir = host.state_dir();
    let mut lines: Vec<String> = Vec::new();
    let report = check(host)?;
    for line in report.lines() {
        host.progress(&line);
        lines.push(line);
    }
    // The release is already linked when a previous run installed it;
    // if the daemon still answers with another build (a restart that did
    // not happen), the run finishes that update instead of stopping at
    // "already up to date".
    let daemon = host.daemon_build()?;
    let finish_restart = report.up_to_date && daemon.as_deref().is_some_and(|b| b != report.target);
    if report.up_to_date && !finish_restart {
        let version = report
            .target_version
            .clone()
            .unwrap_or_else(|| report.target.clone());
        let line = format!("already up to date ({version})");
        host.progress(&line);
        lines.push(line);
        return Ok(RunReport {
            check: report,
            lease: None,
            backup: None,
            install: None,
            drain: json!({"waited": false}),
            restart: Value::Null,
            health: Value::Null,
            rolled_back: false,
            pruned: Vec::new(),
            lines,
        });
    }
    if finish_restart {
        let line = format!(
            "{} is installed, but the daemon still runs {} — finishing the update",
            report.target,
            daemon.as_deref().unwrap_or("nothing")
        );
        host.progress(&line);
        lines.push(line);
    }
    // A lease another identity holds refuses here (never a raw error).
    let lease = take_lease(host, &report.target)?;
    let outcome = run_inner(host, opts, &report, &mut lines, !report.up_to_date);
    // Auto-release: the lease never outlives the update that took it.
    let released = release_lease(host, &lease);
    let _ = host.set_drain(false);
    clear_pending(state_dir);
    let mut report = outcome?;
    report.lines = lines;
    if let Err(e) = released {
        report
            .lines
            .push(format!("warning: the lease was not released: {e}"));
    }
    Ok(report)
}

/// `install`: false when the release is already linked (a previous run
/// installed it and the restart did not happen) — the backup and the
/// install are skipped and the run goes straight to the drain and the
/// switch, with the previous release as the rollback target.
fn run_inner(
    host: &dyn UpdateHost,
    opts: &Options,
    report: &CheckReport,
    lines: &mut Vec<String>,
    install: bool,
) -> Result<RunReport> {
    let target = report.target.clone();
    let mut progress = |host: &dyn UpdateHost, text: String| {
        host.progress(&text);
        lines.push(text);
    };
    let pending = |phase: &str| PendingUpdate {
        phase: phase.to_string(),
        target: target.clone(),
        from: report.current.clone(),
        by: host.identity().to_string(),
        since: host.now(),
    };
    // 1. The marker and the drain gate go up before the backup, so a
    //    crash leaves the fleet quiet, not mid-install.
    host.set_pending(Some(&pending("draining")))?;
    host.set_drain(true)?;
    // 2. The backup (outside the state dir by default), recorded as the
    //    lease receipt, then the install: `upgrade::run` stages the
    //    attested build side by side and moves the link. The daemon
    //    keeps running the old build until the switch below.
    let mut backup = Value::Null;
    let mut install_report = Value::Null;
    if install {
        backup = take_backup(host, opts)?;
        progress(
            host,
            format!(
                "backup: {} (recorded as the lease receipt)",
                backup["backup"]["db"].as_str().unwrap_or("?")
            ),
        );
        install_report = upgrade::run(
            host.source(),
            host.layout(),
            &upgrade::Request {
                target: upgrade::Target::Sha(target.clone()),
                dry_run: false,
                allow_unattested: false,
                // The update took its own backup outside the state dir
                // and recorded it as the lease receipt; upgrade's
                // in-state-dir copy would be a second, weaker one.
                backup_state_dir: None,
            },
        )?;
        progress(
            host,
            format!(
                "install: {target} installed ({}) — the daemon still runs the old build \
                 until the switch",
                install_report["trust"].as_str().unwrap_or("?")
            ),
        );
    }
    // 3. Bounded drain.
    let mut last = String::new();
    let waited = host.now();
    let left = if opts.now {
        host.waiters()?
    } else {
        drain_wait(host, opts.drain.as_secs(), &mut |waiters| {
            let text = render_waiting(waiters);
            if text != last {
                last = text.clone();
                host.progress(&format!("draining: {text}"));
            }
        })?
    };
    let waited_secs = (host.now() - waited).round() as u64;
    let drain = json!({
        "waiters": left.iter().map(|w| json!({
            "alias": w.alias, "message": w.message, "state": w.state, "age_secs": w.age_secs,
        })).collect::<Vec<_>>(),
        "waited_secs": waited_secs,
        "timed_out": !left.is_empty(),
        "now": opts.now,
    });
    if left.is_empty() {
        progress(host, format!("drained: quiet after {waited_secs}s"));
    } else if opts.now {
        progress(
            host,
            format!(
                "--now: switching with {}; interrupted turns resume after the restart",
                render_waiting(&left)
            ),
        );
    } else {
        progress(
            host,
            format!(
                "drain timeout after {waited_secs}s: {} still running; switching anyway, \
                 they resume after the restart",
                render_waiting(&left)
            ),
        );
    }
    host.set_pending(Some(&pending("switching")))?;
    // 4. Switch: the new binary restarts the daemon (and the board).
    let restart = restart_on(host, &target)?;
    // 5. Health check, with auto-rollback to the previous release.
    let previous = if install {
        report.current.clone()
    } else {
        previous_release(host)?
    };
    let health = match health_wait(host, &target, HEALTH_TIMEOUT, || host.board_build()) {
        Ok(()) => json!({"ok": true, "build": target}),
        Err(e) => {
            progress(host, format!("health: {e}"));
            let Some(previous) = previous.clone() else {
                return Err(Error::rejected(format!(
                    "health check failed and there is no previous release to roll back to \
                     ({e}); fix the install by hand, then `cadence update --check`"
                )));
            };
            host.set_pending(Some(&pending("rolling_back")))?;
            progress(host, format!("rollback: repointing to {previous}"));
            upgrade::run(
                host.source(),
                host.layout(),
                &upgrade::Request {
                    target: upgrade::Target::Sha(previous.clone()),
                    dry_run: false,
                    allow_unattested: false,
                    backup_state_dir: None,
                },
            )?;
            let binary = host.layout().binary(&previous);
            host.restart(&binary)?;
            health_wait(host, &previous, HEALTH_TIMEOUT, || host.board_build()).map_err(|e2| {
                Error::rejected(format!(
                    "health check failed on {target} ({e}); the rollback to {previous} did \
                     not come up either ({e2}) — the daemon and board need an operator"
                ))
            })?;
            let restore = backup["backup"]["manifest"].as_str().map(|manifest| {
                format!(
                    "if the schema changed, restore the pre-update backup: \
                     `cadence restore {manifest}`"
                )
            });
            progress(
                host,
                format!(
                    "rolled back: {previous} is answering health again{}",
                    restore
                        .as_deref()
                        .map(|r| format!("; {r}"))
                        .unwrap_or_default()
                ),
            );
            let pruned = prune_releases(host, opts.keep)?;
            return Ok(RunReport {
                check: report.clone(),
                lease: Some(json!({"holder": host.identity(), "released": true})),
                backup: Some(backup),
                install: Some(install_report),
                drain,
                restart,
                health: json!({"ok": false, "build": target, "rolled_back_to": previous,
                               "error": e.to_string()}),
                rolled_back: true,
                pruned,
                lines: Vec::new(),
            });
        }
    };
    progress(host, format!("health: daemon and board answer on {target}"));
    host.set_pending(Some(&pending("verifying")))?;
    let pruned = prune_releases(host, opts.keep)?;
    if !pruned.is_empty() {
        progress(host, format!("releases: pruned {}", pruned.join(", ")));
    }
    let version = version_of(host.layout(), &target).unwrap_or_else(|| target.clone());
    progress(host, format!("done: {version}"));
    Ok(RunReport {
        check: report.clone(),
        lease: Some(json!({"holder": host.identity(), "released": true})),
        backup: Some(backup),
        install: Some(install_report),
        drain,
        restart,
        health,
        rolled_back: false,
        pruned,
        lines: Vec::new(),
    })
}

/// Keep the linked release plus the newest `keep` previous ones; remove
/// the rest. Returns the shas removed. Never touches anything but
/// `<releases>/<40-hex>` directories.
pub fn prune_releases(host: &dyn UpdateHost, keep: usize) -> Result<Vec<String>> {
    let layout = host.layout();
    let linked = match upgrade::current(layout)? {
        Current::Link { sha, .. } => sha,
        _ => None,
    };
    let mut releases = release_dirs(layout)?;
    // Newest first: the linked one is never a prune candidate.
    releases.sort_by_key(|a| std::cmp::Reverse(a.1));
    let mut kept = 0usize;
    let mut removed = Vec::new();
    for (sha, _) in releases {
        if Some(&sha) == linked.as_ref() {
            continue;
        }
        if kept < keep {
            kept += 1;
            continue;
        }
        let dir = layout.release_dir(&sha);
        if std::fs::remove_dir_all(&dir).is_ok() {
            removed.push(sha);
        }
    }
    Ok(removed)
}

/// `<releases>/<40-hex>` directories with their mtimes, newest first.
fn release_dirs(layout: &Layout) -> Result<Vec<(String, std::time::SystemTime)>> {
    let entries = match std::fs::read_dir(&layout.releases) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !upgrade::is_full_sha(&name) || !entry.file_type()?.is_dir() {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        out.push((name, mtime));
    }
    Ok(out)
}

/// The previous release `--rollback` returns to: the newest release
/// directory that is not the linked one.
pub fn previous_release(host: &dyn UpdateHost) -> Result<Option<String>> {
    let linked = match upgrade::current(host.layout())? {
        Current::Link { sha, .. } => sha,
        _ => None,
    };
    let mut releases = release_dirs(host.layout())?;
    releases.sort_by_key(|a| std::cmp::Reverse(a.1));
    Ok(releases
        .into_iter()
        .map(|(sha, _)| sha)
        .find(|sha| Some(sha) != linked.as_ref()))
}

/// `cadence update --rollback`: return to the previous release, restart,
/// health check — and offer the backup restore when the schema changed.
pub fn rollback(host: &dyn UpdateHost) -> Result<RunReport> {
    let layout = host.layout();
    let current = match upgrade::current(layout)? {
        Current::Link { sha, .. } => sha,
        Current::Absent => {
            return Err(Error::rejected(
                "no cadence link is installed — nothing to roll back",
            ))
        }
        Current::NotSymlink => {
            return Err(Error::rejected(format!(
                "{} exists and is not a symlink — refusing to replace it",
                layout.link.display()
            )))
        }
    };
    let Some(previous) = previous_release(host)? else {
        return Err(Error::rejected(format!(
            "no previous release under {} to roll back to",
            layout.releases.display()
        )));
    };
    host.progress(&format!(
        "rolling back: {} → {previous}",
        current.as_deref().unwrap_or("nothing")
    ));
    host.set_pending(Some(&PendingUpdate {
        phase: "rolling_back".to_string(),
        target: previous.clone(),
        from: current.clone(),
        by: host.identity().to_string(),
        since: host.now(),
    }))?;
    let install = upgrade::run(
        host.source(),
        layout,
        &upgrade::Request {
            target: upgrade::Target::Sha(previous.clone()),
            dry_run: false,
            allow_unattested: false,
            backup_state_dir: None,
        },
    )?;
    let binary = layout.binary(&previous);
    host.restart(&binary)?;
    let schema_current = rollout::store_schema(host.state_dir())?;
    let schema_target = host.source().schema_version(&previous).unwrap_or(None);
    let restore = match (schema_current, schema_target) {
        (Some(store), Some(target)) if target < store => Some(format!(
            "the store schema ({store}) is newer than {previous}'s ({target}) — restore the \
             pre-update backup: `cadence restore <manifest>` (see {})",
            default_backup_dir(host.state_dir()).display()
        )),
        _ => None,
    };
    match health_wait(host, &previous, HEALTH_TIMEOUT, || host.board_build()) {
        Ok(()) => {
            host.progress(&format!("health: daemon and board answer on {previous}"));
            if let Some(restore) = &restore {
                host.progress(restore);
            }
            clear_pending(host.state_dir());
            Ok(RunReport {
                check: check_after_rollback(host, &previous),
                lease: None,
                backup: None,
                install: Some(install),
                drain: json!({"waited": false}),
                restart: json!({"binary": binary, "sha": previous}),
                health: json!({"ok": true, "build": previous, "restore": restore}),
                rolled_back: true,
                pruned: Vec::new(),
                lines: Vec::new(),
            })
        }
        Err(e) => Err(Error::rejected(format!(
            "the rollback to {previous} did not come up: {e}{}",
            restore
                .as_deref()
                .map(|r| format!("; {r}"))
                .unwrap_or_default()
        ))),
    }
}

fn check_after_rollback(host: &dyn UpdateHost, sha: &str) -> CheckReport {
    CheckReport {
        current: Some(sha.to_string()),
        current_version: version_of(host.layout(), sha),
        target: sha.to_string(),
        target_version: None,
        up_to_date: true,
        changes: Vec::new(),
        schema_current: None,
        schema_target: None,
        migration: false,
        lease: None,
        waiters: Vec::new(),
        blockers: Vec::new(),
    }
}
