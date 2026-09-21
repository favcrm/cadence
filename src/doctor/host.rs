//! `cadence doctor --host` — a read-only host watchdog for the
//! failures that already bit this project: a provider sqlite WAL that
//! grew to 53 GB and filled the disk (OPS-4), ~2,850 open FIFOs that
//! pushed the user past `fs.pipe-user-pages-soft` so every new pipe
//! came out clamped to one page (CAD-61), test binaries from deleted
//! worktrees still running days later, temp state dirs leaking
//! from test runs, and memory commitment exhaustion — `Committed_AS`
//! three times over `CommitLimit` made `fork()` fail with EAGAIN
//! while every check was green (CAD-154); a per-family process census
//! names the group that ate it (CAD-154) and the daemon checkpoints
//! provider WALs itself while their provider idles (CAD-132).
//!
//! Every check reports `ok | warn | fail` with the measured value, the
//! threshold it was compared against, and a `remedy` — the exact
//! command an operator would run. Nothing here writes, signals or
//! deletes: filesystem reads, `/proc` walks and a handful of read-only
//! `git` probes are the whole surface. The exit code is the worst
//! level: 0 all ok, 1 any warn, 2 any fail.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::proc::run_bounded;

const GIT_TIMEOUT: Duration = Duration::from_secs(15);
const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;
/// A default pipe is 64 KiB = 16 pages; `pipe-user-pages-*` limits are
/// page counts, so a pipe count maps to an estimate through this.
const PAGES_PER_PIPE: u64 = 16;
/// Temp-dir prefixes this project's helpers actually use: tempfile's
/// `.tmp*`, mktemp's `tmp.*` and cadence's own `cadence-*`
/// (`cadence-issue-at-*` exports, leaked state dirs).
const TEMP_PREFIXES: &[&str] = &["cadence-", ".tmp", "tmp."];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One check's verdict; serialises to the `checks[]` entry of `--json`.
struct Check {
    name: &'static str,
    level: Level,
    value: Value,
    threshold: Value,
    detail: String,
    remedy: String,
}

impl Check {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "level": self.level.as_str(),
            "value": self.value,
            "threshold": self.threshold,
            "detail": self.detail,
            "remedy": self.remedy,
        })
    }
}

fn check(
    name: &'static str,
    level: Level,
    value: Value,
    threshold: Value,
    detail: String,
    remedy: String,
) -> Check {
    Check {
        name,
        level,
        value,
        threshold,
        detail,
        remedy,
    }
}

/// `[host]` in `pm.yaml` — every field optional; unset keys keep the
/// built-in defaults. Sizes are bytes, ages are seconds.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostOverrides {
    pub disk_warn_pct: Option<f64>,
    pub disk_warn_free_bytes: Option<u64>,
    pub disk_fail_pct: Option<f64>,
    pub disk_fail_free_bytes: Option<u64>,
    pub wal_warn_bytes: Option<u64>,
    pub wal_fail_bytes: Option<u64>,
    pub store_warn_bytes: Option<u64>,
    /// Fallback for when `fs.pipe-user-pages-soft` cannot be read.
    pub pipe_est_pages_warn: Option<u64>,
    pub orphan_min_age_secs: Option<u64>,
    pub temp_min_age_secs: Option<u64>,
    pub temp_warn_count: Option<u64>,
    pub temp_warn_bytes: Option<u64>,
    /// `MemAvailable` as a percent of `MemTotal` — warn/fail below.
    pub mem_warn_pct: Option<f64>,
    pub mem_fail_pct: Option<f64>,
    /// `SwapFree` as a percent of `SwapTotal` — warn/fail below.
    pub swap_warn_pct: Option<f64>,
    pub swap_fail_pct: Option<f64>,
    /// The daemon checkpoints a provider store's WAL past this size
    /// (CAD-132); `doctor --host` keeps reporting it either way.
    pub wal_max_bytes: Option<u64>,
    /// `wal_checkpoint: false` opts the daemon's WAL watch out
    /// entirely — cadence then never writes to another tool's store.
    pub wal_checkpoint: Option<bool>,
    /// `wal_dry_run: true` records `wal_checkpoint_pending` events for
    /// what the watcher *would* checkpoint instead of touching the db.
    pub wal_dry_run: Option<bool>,
}

/// Every threshold in one place; `pm.yaml [host]` overrides any subset.
#[derive(Clone, Debug)]
pub struct Thresholds {
    pub disk_warn_pct: f64,
    pub disk_warn_free_bytes: u64,
    pub disk_fail_pct: f64,
    pub disk_fail_free_bytes: u64,
    pub wal_warn_bytes: u64,
    pub wal_fail_bytes: u64,
    pub store_warn_bytes: u64,
    pub pipe_est_pages_warn: u64,
    pub orphan_min_age_secs: u64,
    pub temp_min_age_secs: u64,
    pub temp_warn_count: u64,
    pub temp_warn_bytes: u64,
    pub mem_warn_pct: f64,
    pub mem_fail_pct: f64,
    pub swap_warn_pct: f64,
    pub swap_fail_pct: f64,
    pub wal_max_bytes: u64,
    pub wal_checkpoint: bool,
    pub wal_dry_run: bool,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            disk_warn_pct: 15.0,
            disk_warn_free_bytes: 20 * GIB,
            disk_fail_pct: 5.0,
            disk_fail_free_bytes: 5 * GIB,
            wal_warn_bytes: GIB,
            wal_fail_bytes: 10 * GIB,
            store_warn_bytes: 10 * GIB,
            pipe_est_pages_warn: 16_384,
            orphan_min_age_secs: 3_600,
            temp_min_age_secs: 86_400,
            temp_warn_count: 5,
            temp_warn_bytes: 512 * MIB,
            mem_warn_pct: 15.0,
            mem_fail_pct: 5.0,
            swap_warn_pct: 20.0,
            swap_fail_pct: 5.0,
            wal_max_bytes: GIB,
            wal_checkpoint: true,
            wal_dry_run: false,
        }
    }
}

impl Thresholds {
    fn resolve(overrides: Option<HostOverrides>) -> Self {
        let mut t = Self::default();
        if let Some(o) = overrides {
            if let Some(v) = o.disk_warn_pct {
                t.disk_warn_pct = v;
            }
            if let Some(v) = o.disk_warn_free_bytes {
                t.disk_warn_free_bytes = v;
            }
            if let Some(v) = o.disk_fail_pct {
                t.disk_fail_pct = v;
            }
            if let Some(v) = o.disk_fail_free_bytes {
                t.disk_fail_free_bytes = v;
            }
            if let Some(v) = o.wal_warn_bytes {
                t.wal_warn_bytes = v;
            }
            if let Some(v) = o.wal_fail_bytes {
                t.wal_fail_bytes = v;
            }
            if let Some(v) = o.store_warn_bytes {
                t.store_warn_bytes = v;
            }
            if let Some(v) = o.pipe_est_pages_warn {
                t.pipe_est_pages_warn = v;
            }
            if let Some(v) = o.orphan_min_age_secs {
                t.orphan_min_age_secs = v;
            }
            if let Some(v) = o.temp_min_age_secs {
                t.temp_min_age_secs = v;
            }
            if let Some(v) = o.temp_warn_count {
                t.temp_warn_count = v;
            }
            if let Some(v) = o.temp_warn_bytes {
                t.temp_warn_bytes = v;
            }
            if let Some(v) = o.mem_warn_pct {
                t.mem_warn_pct = v;
            }
            if let Some(v) = o.mem_fail_pct {
                t.mem_fail_pct = v;
            }
            if let Some(v) = o.swap_warn_pct {
                t.swap_warn_pct = v;
            }
            if let Some(v) = o.swap_fail_pct {
                t.swap_fail_pct = v;
            }
            if let Some(v) = o.wal_max_bytes {
                t.wal_max_bytes = v;
            }
            if let Some(v) = o.wal_checkpoint {
                t.wal_checkpoint = v;
            }
            if let Some(v) = o.wal_dry_run {
                t.wal_dry_run = v;
            }
        }
        t
    }
}

/// The host under examination — every input is injectable so tests
/// drive the checks from fabricated roots, never real host state.
pub struct Scan {
    pub proc_root: PathBuf,
    pub temp_dir: PathBuf,
    pub home: PathBuf,
    pub state_dir: PathBuf,
    pub cwd: PathBuf,
    pub devin_data: PathBuf,
    pub claude_projects: PathBuf,
    pub codex_sessions: PathBuf,
    pub pm_dir: Option<PathBuf>,
    pub uid: u32,
    pub now: SystemTime,
    pub thresholds: Thresholds,
    pub linux: bool,
    /// Injectable statvfs — tests substitute fabricated free-space
    /// answers so no check ever depends on the host's real disks.
    pub(crate) fs_probe: Option<fn(&Path) -> Option<FsFree>>,
    /// The `/proc` census is one walk per `run` — `memory` consults
    /// it for remedies and `processes` reports it; lazily shared here.
    pub(crate) census: std::cell::OnceCell<Census>,
}

impl Scan {
    /// The real host: `/proc`, the process temp dir, `$HOME`, the cwd's
    /// repo and the optional `[host]` table in `pm.yaml`.
    pub fn host(state_dir: &Path) -> Scan {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let pm_dir = crate::issue::default_dir()
            .ok()
            .filter(|d| d.join("pm.yaml").is_file());
        let thresholds = host_thresholds(pm_dir.as_deref());
        Scan {
            proc_root: PathBuf::from("/proc"),
            temp_dir: std::env::temp_dir(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            devin_data: data_home.join("devin"),
            claude_projects: home.join(".claude/projects"),
            codex_sessions: home.join(".codex/sessions"),
            home,
            state_dir: state_dir.to_path_buf(),
            pm_dir,
            uid: unsafe { libc::geteuid() },
            now: SystemTime::now(),
            thresholds,
            linux: cfg!(target_os = "linux"),
            fs_probe: None,
            census: std::cell::OnceCell::new(),
        }
    }
}

/// The optional `[host]` table in `pm.yaml` — read as plain YAML so a
/// missing or older `pm.yaml` is simply "no overrides", never an error.
fn host_overrides(pm_dir: &Path) -> Option<HostOverrides> {
    let text = std::fs::read_to_string(pm_dir.join("pm.yaml")).ok()?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    serde_yaml::from_value(yaml.get("host")?.clone()).ok()
}

/// Resolved `[host]` thresholds — shared by `Scan::host` and the
/// daemon's WAL watcher so both read one config table.
pub(crate) fn host_thresholds(pm_dir: Option<&Path>) -> Thresholds {
    Thresholds::resolve(pm_dir.and_then(host_overrides))
}

/// All eight checks against `scan`; the report is one JSON object whose
/// `level` is the worst check level.
pub fn run(scan: &Scan) -> Value {
    let checks = [
        check_disk(scan),
        check_provider_state(scan),
        check_pipes(scan),
        check_memory(scan),
        check_processes(scan),
        check_orphans(scan),
        check_temp_dirs(scan),
        check_worktrees(scan),
    ];
    let level = checks.iter().map(|c| c.level).max().unwrap_or(Level::Ok);
    json!({
        "level": level.as_str(),
        "checks": checks.iter().map(Check::to_json).collect::<Vec<_>>(),
    })
}

/// 0 all ok, 1 any warn, 2 any fail.
pub fn exit_code(report: &Value) -> i32 {
    match report["level"].as_str() {
        Some("fail") => 2,
        Some("warn") => 1,
        _ => 0,
    }
}

/// Text form: one line per check, remedies under anything not ok.
pub fn render(report: &Value) -> String {
    let mut out = String::from("cadence doctor --host — read-only host watchdog\n");
    if let Some(checks) = report["checks"].as_array() {
        for c in checks {
            let level = c["level"].as_str().unwrap_or("ok");
            out.push_str(&format!(
                "{level:>5} {:<15} {}\n",
                c["name"].as_str().unwrap_or("?"),
                c["detail"].as_str().unwrap_or("")
            ));
            if level != "ok" {
                let remedy = c["remedy"].as_str().unwrap_or("");
                if !remedy.is_empty() {
                    out.push_str(&format!("       remedy: {remedy}\n"));
                }
            }
        }
    }
    out.push_str(&format!(
        "worst: {} — exit {}\n",
        report["level"].as_str().unwrap_or("ok"),
        exit_code(report)
    ));
    out
}

/// `doctor --host` end to end: scan this host, print the report, exit
/// with the worst level. `--reclaim-plan` runs the same checks and
/// appends what could be freed — a listing, never a deletion — so the
/// command stays safe to swap into a watchdog loop without losing
/// alerting; the exit code is still the worst check level.
pub fn cli(state_dir: &Path, json_out: bool, reclaim: bool) -> Result<i32> {
    let scan = Scan::host(state_dir);
    let report = run(&scan);
    if reclaim {
        let plan = reclaim_plan(&scan);
        if json_out {
            let mut merged = report.clone();
            merged["reclaim"] = plan;
            println!(
                "{}",
                serde_json::to_string_pretty(&merged).unwrap_or_default()
            );
        } else {
            print!("{}", render(&report));
            print!("{}", render_reclaim(&plan));
        }
        return Ok(exit_code(&report));
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        print!("{}", render(&report));
    }
    Ok(exit_code(&report))
}

// ---------- shared helpers ----------

/// Byte sizes for humans: `13.0 GiB`, `512.0 MiB`.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Entries `dir_size` will stat before giving up — a stale worktree
/// with a huge `target/` must not stall the report.
const DIR_WALK_BUDGET: usize = 200_000;

/// Allocated bytes under `path` (`st_blocks`, so sparse files report
/// what they really occupy and `du -sh` reconciles). Descends into
/// real directories only — `ent.metadata()` never follows symlinks,
/// so a lane's shared-cache links are not walked again here. Counts
/// each inode once per call (cargo's hardlinked uplifts can't double
/// up) and stays on the starting path's device, `du -x`-style, so a
/// row's bytes are what `rm -rf` frees *on that filesystem*. Skips
/// anything that vanishes or denies mid-walk — a watchdog walk races
/// with the processes it watches. Returns `(bytes, truncated)`; a
/// truncated walk is a lower bound, not the real size.
fn dir_size(path: &Path) -> (u64, bool) {
    let mut total = 0u64;
    let mut visited = 0_usize;
    let mut inodes = std::collections::HashSet::new();
    let root_dev = std::fs::metadata(path).ok().map(|m| m.dev());
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in entries.flatten() {
            if visited >= DIR_WALK_BUDGET {
                return (total, true);
            }
            visited += 1;
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if root_dev.is_none_or(|d| meta.dev() == d) {
                    stack.push(ent.path());
                }
            } else if inodes.insert((meta.dev(), meta.ino())) {
                total += meta.blocks().saturating_mul(512);
            }
        }
    }
    (total, false)
}

/// POSIX single-quoting for a path emitted inside a shell command —
/// `'a b'` and `'\''`-escaped, so `rm -rf <it>` can never split a
/// path like `/home/ubuntu/My Project` into extra arguments. Paths
/// made of only safe characters print bare for readability.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%_+=:,./-".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The mount point a path lives on (longest-prefix match in
/// `/proc/self/mounts`), so a reclaim row says which filesystem its
/// bytes actually free. `None` off-Linux — the field is omitted.
fn fs_label(path: &Path) -> Option<String> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let mut best: Option<String> = None;
    for line in mounts.lines() {
        let Some(mp) = line.split_whitespace().nth(1) else {
            continue;
        };
        let mp = mp.replace("\\040", " ");
        if canon.starts_with(&mp) && best.as_ref().is_none_or(|b| mp.len() > b.len()) {
            best = Some(mp);
        }
    }
    best
}

/// Is `path` under an exclusive flock right now? A non-blocking
/// LOCK_EX attempt — success means free, and the `File` drop releases
/// the probe lock immediately. Cargo's build-lock files answer "is a
/// lane building" the way cargo itself does.
fn file_locked(path: &Path) -> bool {
    crate::worktree::file_locked(path)
}

/// Bounded read-only `git`; non-zero exits are data, not errors — a
/// merge-base verdict IS the exit code.
fn git_out(dir: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = Command::new("git");
    // --no-optional-locks: `git status` otherwise refreshes .git/index
    // under index.lock — a write, and contention with a worker's `git
    // add`. The watchdog never takes it.
    cmd.arg("--no-optional-locks").arg("-C").arg(dir).args(args);
    run_bounded(&mut cmd, GIT_TIMEOUT).ok()
}

fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git_out(dir, args)?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The main checkout root for `cwd`: `rev-parse --git-common-dir`
/// answers the shared `.git` even from inside a linked worktree.
fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let common = git_stdout(cwd, &["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(&common);
    let common = if common.is_absolute() {
        common
    } else {
        cwd.join(common)
    };
    common.parent().map(|p| p.to_path_buf())
}

fn read_u64_file(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

// ---------- disk ----------

pub(crate) struct FsFree {
    path: PathBuf,
    dev: u64,
    free: u64,
    total: u64,
}

impl FsFree {
    fn pct(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            self.free as f64 * 100.0 / self.total as f64
        }
    }
}

fn fs_free(path: &Path) -> Option<FsFree> {
    let dev = std::fs::metadata(path).ok()?.dev();
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: c_path is a live NUL-terminated string and st is a
    // properly sized out-buffer; statvfs writes into it or returns -1.
    let mut st = unsafe { std::mem::zeroed::<libc::statvfs>() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(FsFree {
        path: path.to_path_buf(),
        dev,
        free: st.f_bavail.saturating_mul(st.f_frsize),
        total: st.f_blocks.saturating_mul(st.f_frsize),
    })
}

/// Pure level for one filesystem — the threshold edges live here.
fn fs_level(free: u64, total: u64, t: &Thresholds) -> Level {
    let pct = if total == 0 {
        100.0
    } else {
        free as f64 * 100.0 / total as f64
    };
    if pct < t.disk_fail_pct || free < t.disk_fail_free_bytes {
        Level::Fail
    } else if pct < t.disk_warn_pct || free < t.disk_warn_free_bytes {
        Level::Warn
    } else {
        Level::Ok
    }
}

fn check_disk(scan: &Scan) -> Check {
    let mut paths = vec![
        scan.state_dir.clone(),
        scan.temp_dir.clone(),
        scan.home.clone(),
    ];
    if let Some(repo) = repo_root(&scan.cwd) {
        paths.push(repo);
    }
    let probe = scan.fs_probe.unwrap_or(fs_free);
    let mut seen = BTreeSet::new();
    let mut fses = Vec::new();
    for path in paths {
        if let Some(f) = probe(&path) {
            if seen.insert(f.dev) {
                fses.push(f);
            }
        }
    }
    eval_disk(&fses, &scan.thresholds)
}

fn eval_disk(fses: &[FsFree], t: &Thresholds) -> Check {
    let name = "disk";
    let threshold = json!(format!(
        "warn: free < {}% or {}; fail: free < {}% or {}",
        t.disk_warn_pct,
        human(t.disk_warn_free_bytes),
        t.disk_fail_pct,
        human(t.disk_fail_free_bytes)
    ));
    if fses.is_empty() {
        return check(
            name,
            Level::Ok,
            json!([]),
            threshold,
            "no filesystems probed".to_string(),
            String::new(),
        );
    }
    let level = fses
        .iter()
        .map(|f| fs_level(f.free, f.total, t))
        .max()
        .unwrap_or(Level::Ok);
    let detail = fses
        .iter()
        .map(|f| {
            format!(
                "{} {:.1}% free ({})",
                f.path.display(),
                f.pct(),
                human(f.free)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let worst = fses.iter().min_by(|a, b| {
        a.pct()
            .partial_cmp(&b.pct())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let remedy = worst
        .map(|f| {
            format!(
                "du -xh --max-depth=1 {} | sort -h  # find the growth before writes fail",
                shell_quote(&f.path.display().to_string())
            )
        })
        .unwrap_or_default();
    let value = fses
        .iter()
        .map(|f| {
            json!({
                "path": f.path,
                "free_bytes": f.free,
                "free_pct": (f.pct() * 10.0).round() / 10.0,
                "level": fs_level(f.free, f.total, t).as_str(),
            })
        })
        .collect::<Vec<_>>();
    check(name, level, json!(value), threshold, detail, remedy)
}

// ---------- provider state growth ----------

struct StoreMeasure {
    label: &'static str,
    path: PathBuf,
    /// Main file bytes, or the dir total for directory stores.
    store_bytes: u64,
    wal_bytes: Option<u64>,
    /// `store_bytes` is a lower bound — the dir walk hit its budget.
    truncated: bool,
    level: Level,
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

fn store_level(store: u64, wal: Option<u64>, t: &Thresholds) -> Level {
    if wal.is_some_and(|w| w > t.wal_fail_bytes) {
        Level::Fail
    } else if wal.is_some_and(|w| w > t.wal_warn_bytes) || store > t.store_warn_bytes {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// One sqlite store (file + `-wal` sibling), absent both = skipped.
fn sqlite_store(label: &'static str, db: &Path, t: &Thresholds) -> Option<StoreMeasure> {
    let store = file_size(db);
    let wal = wal_sibling(db).and_then(|w| file_size(&w));
    if store.is_none() && wal.is_none() {
        return None;
    }
    Some(StoreMeasure {
        label,
        path: db.to_path_buf(),
        store_bytes: store.unwrap_or(0),
        wal_bytes: wal,
        truncated: false,
        level: store_level(store.unwrap_or(0), wal, t),
    })
}

/// A directory store measured by total bytes under it.
fn dir_store(label: &'static str, dir: &Path, t: &Thresholds) -> Option<StoreMeasure> {
    if !dir.is_dir() {
        return None;
    }
    let (bytes, truncated) = dir_size(dir);
    Some(StoreMeasure {
        label,
        path: dir.to_path_buf(),
        store_bytes: bytes,
        wal_bytes: None,
        truncated,
        level: store_level(bytes, None, t),
    })
}

fn check_provider_state(scan: &Scan) -> Check {
    let t = &scan.thresholds;
    let stores: Vec<StoreMeasure> = [
        sqlite_store(
            "devin sessions.db",
            &scan.devin_data.join("cli/sessions.db"),
            t,
        ),
        sqlite_store(
            "cadence sqlite3",
            &scan.state_dir.join("cadence.sqlite3"),
            t,
        ),
        dir_store("claude projects", &scan.claude_projects, t),
        dir_store("codex sessions", &scan.codex_sessions, t),
    ]
    .into_iter()
    .flatten()
    .collect();
    eval_provider_state(&stores, t)
}

fn eval_provider_state(stores: &[StoreMeasure], t: &Thresholds) -> Check {
    let name = "provider-state";
    let threshold = json!(format!(
        "warn: wal > {} or store > {}; fail: wal > {}",
        human(t.wal_warn_bytes),
        human(t.store_warn_bytes),
        human(t.wal_fail_bytes)
    ));
    if stores.is_empty() {
        return check(
            name,
            Level::Ok,
            json!([]),
            threshold,
            "no known provider stores present".to_string(),
            String::new(),
        );
    }
    let level = stores.iter().map(|s| s.level).max().unwrap_or(Level::Ok);
    let detail = stores
        .iter()
        .map(|s| {
            let size = if s.truncated {
                format!("at least {}", human(s.store_bytes))
            } else {
                human(s.store_bytes)
            };
            match s.wal_bytes {
                Some(wal) => format!("{} {} (+wal {})", s.label, size, human(wal)),
                None => format!("{} {}", s.label, size),
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let remedy = stores
        .iter()
        .filter(|s| s.level > Level::Ok)
        .take(2)
        .map(|s| {
            if s.wal_bytes.is_some_and(|w| w > t.wal_warn_bytes) {
                format!(
                    "sqlite3 {} 'PRAGMA wal_checkpoint(TRUNCATE);'",
                    shell_quote(&s.path.display().to_string())
                )
            } else {
                format!(
                    "du -xh --max-depth=1 {} | sort -h",
                    shell_quote(&s.path.display().to_string())
                )
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let value = stores
        .iter()
        .map(|s| {
            json!({
                "store": s.label,
                "path": s.path,
                "store_bytes": s.store_bytes,
                "wal_bytes": s.wal_bytes,
                "level": s.level.as_str(),
                // What the daemon's WAL watch would checkpoint under
                // `[host] wal_max_bytes` — the preview surface.
                "over_checkpoint_limit": s.wal_bytes.is_some_and(|w| w > t.wal_max_bytes),
            })
        })
        .collect::<Vec<_>>();
    check(name, level, json!(value), threshold, detail, remedy)
}

// ---------- pipe pressure (linux) ----------

#[derive(Default)]
struct PipeStats {
    fds: u64,
    pipes: u64,
    est_pages: u64,
    /// Top fifo holders: (pid, count), descending, at most 3.
    top: Vec<(u32, u64)>,
    vanished: u64,
    denied: u64,
}

/// Count `pipe:[inode]` targets under `<proc>/<pid>/fd` for processes
/// owned by `scan.uid`. A pid vanishing or an unreadable fd dir is
/// normal — counted, never fatal.
fn scan_pipes(scan: &Scan) -> PipeStats {
    let mut stats = PipeStats::default();
    let mut inodes = BTreeSet::new();
    let mut counts: BTreeMap<u32, u64> = BTreeMap::new();
    let Ok(pids) = std::fs::read_dir(&scan.proc_root) else {
        return stats;
    };
    for ent in pids.flatten() {
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        // The pipe-user-pages limit is per-user: only this user's pids.
        let Ok(meta) = ent.metadata() else {
            stats.vanished += 1;
            continue;
        };
        if meta.uid() != scan.uid {
            continue;
        }
        let fds = match std::fs::read_dir(ent.path().join("fd")) {
            Ok(fds) => fds,
            Err(e) => {
                match e.kind() {
                    std::io::ErrorKind::NotFound => stats.vanished += 1,
                    std::io::ErrorKind::PermissionDenied => stats.denied += 1,
                    _ => {}
                }
                continue;
            }
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let text = target.to_string_lossy();
            if let Some(inode) = text
                .strip_prefix("pipe:[")
                .and_then(|s| s.strip_suffix(']'))
            {
                stats.fds += 1;
                inodes.insert(inode.to_string());
                *counts.entry(pid).or_default() += 1;
            }
        }
    }
    stats.pipes = inodes.len() as u64;
    stats.est_pages = stats.pipes * PAGES_PER_PIPE;
    let mut top: Vec<(u32, u64)> = counts.into_iter().collect();
    top.sort_by_key(|e| std::cmp::Reverse(e.1));
    top.truncate(3);
    stats.top = top;
    stats
}

fn check_pipes(scan: &Scan) -> Check {
    let name = "pipes";
    let t = &scan.thresholds;
    let threshold = json!("warn: est. pipe pages > fs.pipe-user-pages-soft");
    if !scan.linux {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "pipe pressure is linux-only".to_string(),
            String::new(),
        );
    }
    let sys = scan.proc_root.join("sys/fs");
    let soft = read_u64_file(&sys.join("pipe-user-pages-soft")).unwrap_or(t.pipe_est_pages_warn);
    let max_size = read_u64_file(&sys.join("pipe-max-size"));
    let stats = scan_pipes(scan);
    let clamped = stats.est_pages > soft;
    let level = if clamped { Level::Warn } else { Level::Ok };
    let mut detail = format!(
        "{} pipes on {} fds (~{} pages, soft {})",
        stats.pipes, stats.fds, stats.est_pages, soft
    );
    if clamped {
        detail.push_str(" — new pipes clamp to one page");
    }
    if let Some(max) = max_size {
        detail.push_str(&format!("; pipe-max-size {}", human(max)));
        if max < MIB {
            detail.push_str(" (already small)");
        }
    }
    if !stats.top.is_empty() {
        detail.push_str(&format!(
            "; top: {}",
            stats
                .top
                .iter()
                .map(|(pid, n)| format!("pid {pid} ×{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if stats.denied + stats.vanished > 0 {
        detail.push_str(&format!(
            "; {} unreadable, {} vanished mid-scan",
            stats.denied, stats.vanished
        ));
    }
    let remedy = if stats.top.is_empty() {
        String::new()
    } else {
        format!(
            "kill {}  # the biggest FIFO holders release the user's pipe pages",
            stats
                .top
                .iter()
                .map(|(pid, _)| pid.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let value = json!({
        "pipe_fds": stats.fds,
        "unique_pipes": stats.pipes,
        "est_pages": stats.est_pages,
        "soft_limit_pages": soft,
        "pipe_max_size_bytes": max_size,
        "clamped": clamped,
        "top": stats.top.iter().map(|(pid, n)| json!({"pid": pid, "fds": n})).collect::<Vec<_>>(),
        "unreadable": stats.denied,
        "vanished": stats.vanished,
    });
    check(name, level, value, threshold, detail, remedy)
}

// ---------- memory commitment + process census (linux) ----------

/// `/proc/meminfo`, the fields the watchdog needs. `Option` fields
/// distinguish "absent" from a real zero — a kernel too old for
/// `MemAvailable` must not read as "0 bytes free".
#[derive(Default)]
struct MemInfo {
    total: u64,
    available: Option<u64>,
    swap_total: Option<u64>,
    swap_free: Option<u64>,
    committed: Option<u64>,
    commit_limit: Option<u64>,
}

/// Parse `Key: NNN kB` lines from `proc_root/meminfo`. `None` when the
/// file is unreadable or `MemTotal` is missing.
fn read_meminfo(proc_root: &Path) -> Option<MemInfo> {
    let text = std::fs::read_to_string(proc_root.join("meminfo")).ok()?;
    let mut m = MemInfo::default();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(kb) = rest
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        let bytes = kb.saturating_mul(1024);
        match key.trim() {
            "MemTotal" => m.total = bytes,
            "MemAvailable" => m.available = Some(bytes),
            "SwapTotal" => m.swap_total = Some(bytes),
            "SwapFree" => m.swap_free = Some(bytes),
            "Committed_AS" => m.committed = Some(bytes),
            "CommitLimit" => m.commit_limit = Some(bytes),
            _ => {}
        }
    }
    (m.total > 0).then_some(m)
}

/// `/proc/<pid>/stat`: (comm, utime+stime jiffies, starttime jiffies,
/// rss bytes). `comm` is the kernel name — the census groups on it;
/// fields after the last `)` are positional and safe.
fn proc_stat(pid_dir: &Path) -> Option<(String, u64, u64, u64)> {
    let text = std::fs::read_to_string(pid_dir.join("stat")).ok()?;
    let (head, rest) = text.rsplit_once(')')?;
    let comm = head.split_once('(')?.1.trim().to_string();
    let f: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    let start_jiffies: u64 = f.get(19)?.parse().ok()?;
    let rss_pages: i64 = f.get(21)?.parse().ok()?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
    let rss = if rss_pages > 0 {
        (rss_pages as u64).saturating_mul(page)
    } else {
        0
    };
    Some((comm, utime.saturating_add(stime), start_jiffies, rss))
}

/// Coalesce comm variants into the family an operator thinks in —
/// `chrome`, `chrome_crashpad` and `chrome-sandbox` are one group.
fn comm_family(comm: &str) -> String {
    let c = comm.trim_end_matches("(deleted)").trim().to_lowercase();
    for family in [
        "chrome",
        "chromium",
        "firefox",
        "node",
        "deno",
        "cargo",
        "rustc",
        "rust-analyzer",
        "claude",
        "devin",
        "codex",
        "cadence",
        "python",
        "tmux",
        "postgres",
        "redis",
    ] {
        // Exact match or a `-`/`_` boundary — `nodemon` is not `node`,
        // `chrome_crashpad` and `chrome-sandbox` are chrome.
        if c == *family
            || c.strip_prefix(family)
                .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('_'))
        {
            return family.to_string();
        }
    }
    c
}

/// The oldest process seen in a group, for the census line.
#[derive(Clone)]
struct OldestProc {
    pid: u32,
    age_secs: u64,
    cpu_secs: u64,
    idle: bool,
}

#[derive(Default)]
struct GroupAgg {
    count: u64,
    rss_bytes: u64,
    uids: BTreeSet<u32>,
    /// Oldest process overall.
    oldest: Option<OldestProc>,
    /// Oldest *idle* process — long-lived at near-zero CPU is the
    /// leaked-session shape CAD-154 watches for.
    oldest_idle: Option<OldestProc>,
}

/// One pass over `proc_root` grouping every readable pid by comm
/// family — all users, not just ours: the leaked sessions that
/// starved this host were root's.
#[derive(Default)]
pub(crate) struct Census {
    groups: BTreeMap<String, GroupAgg>,
    procs: u64,
    unreadable: u64,
    vanished: u64,
}

/// The shared census — one `/proc` walk per `run`, reused by the
/// `processes` check and any `memory` remedy in the same report.
fn census_of(scan: &Scan) -> &Census {
    scan.census.get_or_init(|| proc_census(scan))
}

fn proc_census(scan: &Scan) -> Census {
    let mut census = Census::default();
    let uptime = proc_uptime(&scan.proc_root);
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let Ok(pids) = std::fs::read_dir(&scan.proc_root) else {
        return census;
    };
    for ent in pids.flatten() {
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(meta) = ent.metadata() else {
            census.vanished += 1;
            continue;
        };
        let Some((comm, cpu_jiffies, start_jiffies, rss)) = proc_stat(&ent.path()) else {
            // A pid that vanished mid-scan is normal; an unreadable
            // stat is hidepid or a race — counted either way.
            if ent.path().exists() {
                census.unreadable += 1;
            } else {
                census.vanished += 1;
            }
            continue;
        };
        let age = uptime.map(|u| (u as u64).saturating_sub(start_jiffies / hz));
        let cpu_secs = cpu_jiffies / hz;
        // "Idle": alive over an hour at under ~1% duty — the leaked
        // browser sessions of CAD-154 burned nothing for days.
        let idle =
            age.is_some_and(|a| a >= 3_600) && cpu_secs.saturating_mul(100) <= age.unwrap_or(0);
        let group = census.groups.entry(comm_family(&comm)).or_default();
        group.count += 1;
        group.rss_bytes += rss;
        group.uids.insert(meta.uid());
        census.procs += 1;
        if let Some(age_secs) = age {
            let proc = OldestProc {
                pid,
                age_secs,
                cpu_secs,
                idle,
            };
            if group.oldest.as_ref().is_none_or(|o| age_secs > o.age_secs) {
                group.oldest = Some(proc.clone());
            }
            if idle
                && group
                    .oldest_idle
                    .as_ref()
                    .is_none_or(|o| age_secs > o.age_secs)
            {
                group.oldest_idle = Some(proc);
            }
        }
    }
    census
}

/// Top `n` groups by resident bytes — the remedy names these.
fn top_groups(census: &Census, n: usize) -> Vec<(&String, &GroupAgg)> {
    let mut groups: Vec<(&String, &GroupAgg)> = census.groups.iter().collect();
    groups.sort_by_key(|(_, g)| std::cmp::Reverse(g.rss_bytes));
    groups.truncate(n);
    groups
}

/// `"chrome ×28 12.4 GiB (oldest 50h idle)"` — one group's census line.
fn group_line(name: &str, g: &GroupAgg) -> String {
    let mut out = format!("{name} ×{} {}", g.count, human(g.rss_bytes));
    // The idle oldest tells the leak story when there is one.
    let oldest = g.oldest_idle.as_ref().or(g.oldest.as_ref());
    if let Some(o) = oldest {
        out.push_str(&format!(
            " (oldest {}h, pid {}{}{})",
            o.age_secs / 3600,
            o.pid,
            if o.idle { ", idle" } else { "" },
            if g.uids.len() == 1 && g.uids.contains(&0) {
                ", as root"
            } else {
                ""
            }
        ));
    }
    out
}

/// Memory pressure: `MemAvailable` and `SwapFree` against their
/// thresholds, `Committed_AS` against `CommitLimit` with the
/// overcommit mode named. Commitment over the limit is the CAD-154
/// failure — `fork()`/`malloc` refusal reads as EAGAIN, not ENOMEM.
fn check_memory(scan: &Scan) -> Check {
    let name = "memory";
    let t = &scan.thresholds;
    let threshold = json!(format!(
        "warn: available <{}% RAM or swap free <{}%; fail: available <{}% \
         or (swap free <{}% with available <{}%); committed > limit is a \
         fail only under strict overcommit (mode 2)",
        t.mem_warn_pct, t.swap_warn_pct, t.mem_fail_pct, t.swap_fail_pct, t.mem_warn_pct
    ));
    if !scan.linux {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "memory pressure is linux-only".to_string(),
            String::new(),
        );
    }
    let Some(mem) = read_meminfo(&scan.proc_root) else {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "no readable meminfo".to_string(),
            String::new(),
        );
    };
    let overcommit = read_u64_file(&scan.proc_root.join("sys/vm/overcommit_memory"));
    let mut level = Level::Ok;

    let mut parts = Vec::new();
    // `avail_low` feeds the combined swap leg — swap exhaustion alone
    // is a warning; swap exhaustion *with* low MemAvailable is the
    // CAD-154 incident shape and is the fail.
    let mut avail_low = false;
    if let Some(avail) = mem.available {
        let pct = avail as f64 * 100.0 / mem.total as f64;
        avail_low = pct < t.mem_warn_pct;
        let leg = if pct < t.mem_fail_pct {
            Level::Fail
        } else if avail_low {
            Level::Warn
        } else {
            Level::Ok
        };
        level = level.max(leg);
        parts.push(format!(
            "available {} ({pct:.0}% of {})",
            human(avail),
            human(mem.total)
        ));
    } else {
        parts.push("MemAvailable absent".to_string());
    }
    match (mem.swap_total, mem.swap_free) {
        (Some(total), Some(free)) if total > 0 => {
            let pct = free as f64 * 100.0 / total as f64;
            // Swap exhausted while RAM is still available is the
            // steady state of a long-lived host — warn, not fail. The
            // fail needs both legs of the incident: swap <fail AND
            // MemAvailable already low. With MemAvailable absent the
            // other half can't be seen, so swap alone stays the vote.
            let leg = if pct < t.swap_fail_pct && (avail_low || mem.available.is_none()) {
                Level::Fail
            } else if pct < t.swap_warn_pct {
                Level::Warn
            } else {
                Level::Ok
            };
            level = level.max(leg);
            parts.push(format!(
                "swap free {} ({pct:.0}% of {})",
                human(free),
                human(total)
            ));
        }
        (Some(0), _) | (None, _) => parts.push("no swap".to_string()),
        _ => {}
    }
    // Committed_AS vs CommitLimit is only a hard signal under strict
    // overcommit (mode 2), where the kernel refuses once the limit
    // passes. Under the default heuristic (0) CommitLimit is advisory
    // — Committed_AS routinely exceeds it on a healthy host — and
    // mode 1 never enforces. An unreadable sysctl cannot prove
    // enforcement is off, so it warns rather than fails.
    let mut commit_over = false;
    if let (Some(committed), Some(limit)) = (mem.committed, mem.commit_limit) {
        commit_over = committed > limit;
        let mode = match overcommit {
            Some(0) => "heuristic",
            Some(1) => "always",
            Some(2) => "strict",
            Some(_) | None => "unknown",
        };
        parts.push(format!(
            "committed {} vs limit {} (overcommit_memory={mode})",
            human(committed),
            human(limit),
        ));
        if commit_over {
            level = level.max(match overcommit {
                Some(2) => Level::Fail,
                None => Level::Warn,
                _ => Level::Ok,
            });
        }
    }
    let mut detail = parts.join("; ");
    if commit_over && overcommit == Some(2) {
        detail.push_str(" — fork()/malloc refused (strict overcommit)");
    }
    // The remedy names the biggest process groups — counts, ages,
    // resident bytes — and never kills anything itself.
    let remedy = if level > Level::Ok {
        let groups = top_groups(census_of(scan), 3);
        if groups.is_empty() {
            "inspect `ps aux --sort=-rss | head` — cadence never kills".to_string()
        } else {
            format!(
                "largest groups: {}; restart or close the offenders — cadence never kills",
                groups
                    .iter()
                    .map(|(name, g)| group_line(name, g))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        }
    } else {
        String::new()
    };
    let value = json!({
        "mem_total_bytes": mem.total,
        "mem_available_bytes": mem.available,
        "swap_total_bytes": mem.swap_total,
        "swap_free_bytes": mem.swap_free,
        "committed_bytes": mem.committed,
        "commit_limit_bytes": mem.commit_limit,
        "committed_over_limit": commit_over,
        "overcommit_memory": overcommit,
    });
    check(name, level, value, threshold, detail, remedy)
}

/// The process-group census — informational: per-family counts, total
/// resident bytes and the oldest idle instance, so a leaked session
/// is visible before it starves the host. Never alarms; `memory`
/// carries the thresholds.
fn check_processes(scan: &Scan) -> Check {
    let name = "processes";
    let threshold = json!("informational — no threshold");
    if !scan.linux {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "process census is linux-only".to_string(),
            String::new(),
        );
    }
    let census = census_of(scan);
    let top = top_groups(census, 5);
    let mut detail = format!("{} procs", census.procs);
    if !top.is_empty() {
        detail.push_str(&format!(
            ": {}",
            top.iter()
                .map(|(name, g)| group_line(name, g))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if census.unreadable + census.vanished > 0 {
        detail.push_str(&format!(
            "; {} unreadable, {} vanished mid-scan",
            census.unreadable, census.vanished
        ));
    }
    let groups: Vec<Value> = census
        .groups
        .iter()
        .map(|(name, g)| {
            json!({
                "group": name,
                "count": g.count,
                "rss_bytes": g.rss_bytes,
                "uids": g.uids,
                "oldest": g.oldest.as_ref().map(|o| json!({
                    "pid": o.pid,
                    "age_secs": o.age_secs,
                    "cpu_secs": o.cpu_secs,
                    "idle": o.idle,
                })),
                "oldest_idle": g.oldest_idle.as_ref().map(|o| json!({
                    "pid": o.pid,
                    "age_secs": o.age_secs,
                    "cpu_secs": o.cpu_secs,
                })),
            })
        })
        .collect();
    let value = json!({
        "procs": census.procs,
        "groups": groups,
        "unreadable": census.unreadable,
        "vanished": census.vanished,
    });
    check(name, Level::Ok, value, threshold, detail, String::new())
}

// ---------- provider WAL roots (shared with the daemon watcher) ----------

/// One provider's sqlite watch root: `*-wal` files anywhere under it
/// are checkpoint candidates. The daemon watches these; the provider
/// name is what the "no live turn" gate checks.
pub(crate) struct WalRoot {
    pub provider: &'static str,
    pub label: &'static str,
    pub root: PathBuf,
}

/// The roots the daemon's WAL watcher scans — the same provider
/// stores `provider-state` reports on (devin `sessions.db`, the codex
/// dir's `*.sqlite`, claude projects' nested dbs).
pub(crate) fn wal_roots(home: &Path, data_home: &Path) -> Vec<WalRoot> {
    vec![
        WalRoot {
            provider: "devin",
            label: "devin sessions.db",
            root: data_home.join("devin/cli"),
        },
        WalRoot {
            provider: "codex",
            label: "codex sessions",
            root: home.join(".codex"),
        },
        WalRoot {
            provider: "claude",
            label: "claude projects",
            root: home.join(".claude/projects"),
        },
    ]
}

/// The `<db>-wal` sibling cadence and sqlite both write next to the
/// main file.
pub(crate) fn wal_sibling(db: &Path) -> Option<PathBuf> {
    Some(db.with_file_name(format!("{}-wal", db.file_name()?.to_string_lossy())))
}

/// What `find_wals` walked to. `truncated` is the honest signal that
/// the caps stopped the walk before every `*-wal` was reached — a
/// `~/.claude/projects` on a busy host is exactly the store that can
/// exceed them.
#[derive(Default)]
pub(crate) struct WalScan {
    pub dbs: Vec<PathBuf>,
    pub truncated: bool,
}

/// `*.db`/`*.sqlite`/`*.sqlite3` WAL siblings under `root`, returning
/// the DB paths (a `*-wal` file's presence means the store is in WAL
/// mode already). Depth-capped; the *matched-db* count is capped so a
/// dirent-heavy root can't starve the match set, and truncation is
/// reported rather than silent.
pub(crate) fn find_wals(root: &Path) -> WalScan {
    const MAX_DEPTH: u8 = 4;
    const MAX_DBS: usize = 1_024;
    // Dirent safety bound, far above any real store: a runaway walk
    // must not stall the daemon's tick, but hitting this reports
    // truncation instead of quietly missing the fat WAL.
    const MAX_VISITED: usize = 65_536;
    let mut scan = WalScan::default();
    let mut stack = vec![(root.to_path_buf(), 0_u8)];
    let mut visited = 0_usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in entries.flatten() {
            if scan.dbs.len() >= MAX_DBS || visited >= MAX_VISITED {
                scan.truncated = true;
                return scan;
            }
            visited += 1;
            // DirEntry::metadata does not follow links — a symlinked
            // dir inside a root is skipped rather than descended.
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((ent.path(), depth + 1));
                }
                continue;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix("-wal") else {
                continue;
            };
            if stem.ends_with(".db") || stem.ends_with(".sqlite") || stem.ends_with(".sqlite3") {
                scan.dbs.push(dir.join(stem));
            }
        }
    }
    scan
}

// ---------- orphaned work ----------

struct Orphan {
    pid: u32,
    age_secs: Option<u64>,
    head: String,
    reasons: Vec<&'static str>,
}

/// `readlink` targets append " (deleted)" when the inode is gone.
/// Match a path inside a `.cadence/wt/` tree that no longer exists.
fn deleted_worktree(target: &Path) -> Option<PathBuf> {
    let text = target.to_string_lossy();
    let stripped = text.strip_suffix(" (deleted)").unwrap_or(&text);
    let in_wt = stripped.contains("/.cadence/wt/") || stripped.ends_with("/.cadence/wt");
    let path = PathBuf::from(stripped);
    (in_wt && !path.exists()).then_some(path)
}

/// Cargo test binaries live at `target/{debug,release}/deps/<name>-<hash>`.
fn is_test_binary(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let stripped = text.strip_suffix(" (deleted)").unwrap_or(&text);
    stripped.contains("/target/debug/deps/") || stripped.contains("/target/release/deps/")
}

/// `/proc/uptime`'s first field, seconds since boot.
fn proc_uptime(proc_root: &Path) -> Option<f64> {
    std::fs::read_to_string(proc_root.join("uptime"))
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Age from `/proc/<pid>/stat` field 22 (starttime, jiffies since boot).
fn pid_age_secs(pid_dir: &Path, uptime: Option<f64>) -> Option<u64> {
    let text = std::fs::read_to_string(pid_dir.join("stat")).ok()?;
    // comm may hold spaces/parens — fields after the last ')' are safe.
    let after_comm = text.rsplit_once(')')?.1;
    let start_jiffies: u64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let started = start_jiffies / hz;
    Some((uptime? as u64).saturating_sub(started))
}

/// The only text a secret value is ever replaced by.
const REDACTED: &str = "[REDACTED]";

/// Words that mark an argument name as credential-bearing —
/// `--figma-api-key`, `GITHUB_TOKEN`, `PGPASSWORD`, `DATABASE_URL`,
/// `SENTRY_DSN`, `SLACK_WEBHOOK`. A keyword only counts at a word
/// boundary (`-`, `_` or the start of the name): `monkey`, `keyboard`
/// and npm's `--access` stay ordinary while `api-key`,
/// `aws_secret_access_key` and `access_token` still trip it.
const SECRET_WORDS: &[&str] = &[
    "key", "token", "secret", "pass", "auth", "bearer", "cred", "url", "dsn", "webhook", "conn",
];

fn secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // Inside a SHOUTED name words concatenate without separators
    // (`PGPASSWORD`, `AWSACCESSKEYID`) — anywhere counts.
    let shouted = !n.is_empty() && name.chars().all(|c| !c.is_ascii_lowercase());
    SECRET_WORDS.iter().any(|kw| {
        n.match_indices(kw)
            .any(|(i, _)| shouted || i == 0 || matches!(n.as_bytes()[i - 1], b'-' | b'_'))
    })
}

/// `NAME=value` env-name shape — `FOO_1` yes, `x?y` no.
fn env_name(name: &str) -> bool {
    let mut c = name.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_')
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Argv text that is not a credential on its face — git SHAs (7, 8,
/// 40 or 64 hex chars) and canonical UUIDs are the ordinary long
/// tokens in process lists and must survive.
fn exempt_plain(s: &str) -> bool {
    let hex = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_hexdigit());
    if matches!(s.len(), 7 | 8 | 40 | 64) && hex(s) {
        return true;
    }
    let segs: Vec<&str> = s.split('-').collect();
    segs.len() == 5
        && segs
            .iter()
            .zip([8_usize, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && hex(p))
}

/// Credential shapes — provider prefixes (Figma `figd_`, GitHub
/// `ghp_`/`gho_`/`github_pat_`, `sk-`, Slack `xox[abpr]-`), AWS
/// `AKIA…`, JWTs, and 32+ char high-entropy tokens. `slash_ok` widens
/// the entropy charset to base64 (`/`, `+`) for a value under a flag
/// or env name — AWS secret access keys carry both and can be
/// digit-free — while a standalone arg with `/` stays classed as a
/// path and keeps the strict charset.
fn credential_shape(s: &str, slash_ok: bool) -> bool {
    const PREFIXES: &[&str] = &[
        "figd_",
        "ghp_",
        "gho_",
        "github_pat_",
        "sk-",
        "xoxa-",
        "xoxb-",
        "xoxp-",
        "xoxr-",
    ];
    if exempt_plain(s) {
        return false;
    }
    if PREFIXES.iter().any(|p| s.starts_with(p)) {
        return true;
    }
    // AWS access key id: `AKIA` + 16 uppercase/digits, exactly.
    if s.len() == 20
        && s.starts_with("AKIA")
        && s[4..]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return true;
    }
    // JWT: `eyJ…`.`…`.`…` — three non-empty base64url segments.
    if s.starts_with("eyJ") {
        let segs: Vec<&str> = s.split('.').collect();
        if segs.len() == 3
            && segs.iter().all(|seg| {
                !seg.is_empty()
                    && seg
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
        {
            return true;
        }
    }
    // 32+ char high-entropy token. Strict charset for standalone args
    // (`/` means path); a flag/env value may use the full base64 set,
    // where a `/` or `+` alone satisfies the mix requirement — but an
    // absolute path is never a token.
    s.len() >= 32
        && !s.starts_with('-')
        && !(slash_ok && s.starts_with('/'))
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '~' | '+' | '=')
                || (slash_ok && c == '/')
        })
        && s.chars().any(|c| c.is_ascii_alphabetic())
        && (s.chars().any(|c| c.is_ascii_digit())
            || (slash_ok && (s.contains('/') || s.contains('+'))))
}

fn looks_like_credential(arg: &str) -> bool {
    credential_shape(arg, false)
}

/// The value half of `name=value` or `--flag=value`: the standalone
/// shapes plus the base64 set.
fn secret_value(s: &str) -> bool {
    credential_shape(s, true)
}

/// `user:pass` userinfo shape — `-u admin:hunter2` yes, `-u root`,
/// `8080:80` port maps and `127.0.0.1:8080` no.
fn user_pass_shape(s: &str) -> bool {
    let Some((u, p)) = s.split_once(':') else {
        return false;
    };
    !u.is_empty()
        && !p.is_empty()
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '%'))
        && !(portish(u) && portish(p))
}

/// Pure digits/dots/colons — ports and port maps (`2222`, `8080:80`,
/// `127.0.0.1:8080:80`) that `-p` carries for ssh and docker, not a
/// password.
fn portish(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ':'))
}

/// `scheme://user:<secret>@host` inside any text — the password half
/// of URI userinfo becomes `[REDACTED]`; a credential-shaped
/// username-only userinfo is redacted whole.
fn scrub_uri(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        out.push_str(&rest[..i + 3]);
        let after = &rest[i + 3..];
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let netloc = &after[..end];
        match netloc.find('@') {
            Some(at) => {
                let ui = &netloc[..at];
                match ui.find(':') {
                    Some(c) => {
                        out.push_str(&ui[..c + 1]);
                        out.push_str(REDACTED);
                    }
                    None if looks_like_credential(ui) => out.push_str(REDACTED),
                    _ => out.push_str(ui),
                }
                out.push('@');
                out.push_str(&netloc[at + 1..]);
            }
            None => out.push_str(netloc),
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// `Key: value` scrubbing for an argument or `name=` value that
/// survived the name tests: a `:` whose left side names a secret
/// (`Authorization: Basic …`) or whose right side holds a credential
/// shape (`X-Custom: figd_…`) redacts the remainder of the argument,
/// spaces included; then URI userinfo loses its password.
fn scrub_tail(s: &str) -> String {
    let colon_scrubbed = match s.split_once(':') {
        Some((left, right))
            if secret_name(left)
                || right.split_whitespace().any(|seg| {
                    secret_value(seg.trim_matches(|c: char| matches!(c, '"' | '\'')))
                }) =>
        {
            format!("{left}: [REDACTED]")
        }
        _ => s.to_string(),
    };
    scrub_uri(&colon_scrubbed)
}

/// True when `arg` is itself a secret-bearing flag — the next-arg
/// consumer must not eat another flag's name as its value
/// (`--token --password x` still redacts `x`).
fn is_secret_flag(arg: &str) -> bool {
    if !arg.starts_with('-') || arg.len() < 2 {
        return false;
    }
    let body = arg.trim_start_matches('-');
    let name = body.split('=').next().unwrap_or(body);
    secret_name(name) || (!arg.starts_with("--") && matches!(name, "p" | "a" | "u"))
}

/// Redact credential material from an argv, returning it joined with
/// spaces — the one place argv becomes display text. The executable
/// and ordinary arguments pass through; secret *values* become
/// `[REDACTED]`: the value of any `--flag=value` or `--flag value`
/// whose flag name looks credential-bearing, `NAME=value` env-style
/// arguments with such a name, `Key: value` header arguments, the
/// password half of `scheme://user:pass@host`, `-p`/`-a`/`-u`
/// short-flag values, and any standalone argument matching a known
/// credential shape or the 32+ char high-entropy token shape. Public
/// because `src/session.rs` (landing with PR #63, in review) will
/// share it once both land — argv-as-text must share one scrubber
/// rather than re-implement.
pub fn redact_argv<S: AsRef<str>>(argv: &[S]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_ref();
        if arg.starts_with('-') && arg.len() > 1 {
            // A flag: `--name=value`, `--name value`, `-x<value>` or a
            // bare switch.
            let body = arg.trim_start_matches('-');
            match body.split_once('=') {
                Some((name, value)) => {
                    if secret_name(name) || secret_value(value) {
                        out.push(format!(
                            "{}={REDACTED}",
                            &arg[..arg.len() - value.len() - 1]
                        ));
                    } else {
                        out.push(format!(
                            "{}={}",
                            &arg[..arg.len() - value.len() - 1],
                            scrub_tail(value)
                        ));
                    }
                }
                None => {
                    if secret_name(body) {
                        out.push(arg.to_string());
                        // A secret flag eats the next argument as its
                        // value even when it starts with `-`
                        // (`--password -p123`) — unless it is itself
                        // a secret flag (`--token --password x`).
                        if argv.get(i + 1).is_some_and(|v| !is_secret_flag(v.as_ref())) {
                            out.push(REDACTED.to_string());
                            i += 1;
                        }
                    } else if !arg.starts_with("--") {
                        let c = body.chars().next().unwrap();
                        if body.len() > c.len_utf8() {
                            // `-p<pass>` (mysql), `-u<user:pass>`
                            // (curl), `-x<token>` when the tail is a
                            // credential shape.
                            let tail = &body[c.len_utf8()..];
                            let secret_tail = match c {
                                'p' => !portish(tail),
                                'u' => user_pass_shape(tail),
                                _ => looks_like_credential(tail),
                            };
                            if secret_tail {
                                out.push(format!("-{c}{REDACTED}"));
                            } else {
                                out.push(arg.to_string());
                            }
                        } else {
                            // Bare single-letter flag: `-p`/`-a`
                            // take a value, `-u` takes `user:pass`.
                            out.push(arg.to_string());
                            let take_next = match body {
                                "p" | "a" => argv.get(i + 1).is_some_and(|v| {
                                    !v.as_ref().starts_with('-') && !portish(v.as_ref())
                                }),
                                "u" => argv.get(i + 1).is_some_and(|v| user_pass_shape(v.as_ref())),
                                _ => false,
                            };
                            if take_next {
                                out.push(REDACTED.to_string());
                                i += 1;
                            }
                        }
                    } else {
                        out.push(arg.to_string());
                    }
                }
            }
        } else {
            // A `:` whose left side names a secret redacts the rest of
            // the argument, spaces included — `Authorization: Basic …`
            // arrives as one argv element. Only when the `:` precedes
            // any `=` — a `NAME=value` arg keeps its env form.
            let colon_first = match (arg.find(':'), arg.find('=')) {
                (Some(c), Some(e)) => c < e,
                (Some(_), None) => true,
                _ => false,
            };
            if colon_first {
                let (left, right) = arg.split_once(':').unwrap();
                if secret_name(left)
                    || right.split_whitespace().any(|seg| {
                        secret_value(seg.trim_matches(|c: char| matches!(c, '"' | '\'')))
                    })
                {
                    out.push(format!("{left}: [REDACTED]"));
                    i += 1;
                    continue;
                }
            }
            if let Some((name, value)) = arg.split_once('=') {
                // `NAME=value` env-style, or any `x=y` whose value is
                // a credential shape; the header/URI rules apply
                // inside the value too.
                if (env_name(name) && secret_name(name)) || secret_value(value) {
                    out.push(format!("{name}={REDACTED}"));
                } else {
                    out.push(format!("{name}={}", scrub_tail(value)));
                }
            } else if looks_like_credential(arg) {
                out.push(REDACTED.to_string());
            } else {
                out.push(scrub_tail(arg));
            }
        }
        i += 1;
    }
    out.join(" ")
}

/// argv[0] plus a redacted, ~120-char head of the full command line —
/// test-binary detection needs the untruncated argv0, and the head is
/// display text so `redact_argv` scrubs it before anything stores it.
fn cmdline(pid_dir: &Path) -> (Option<PathBuf>, Option<String>) {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(pid_dir.join("cmdline")) else {
        return (None, None);
    };
    // argv can be huge — 8 KiB is far past the 120 characters kept.
    let mut raw = Vec::new();
    if file.take(8 * 1024).read_to_end(&mut raw).is_err() {
        return (None, None);
    }
    let mut parts: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    // The file ends with a NUL — drop only that trailing empty so
    // interior empty arguments stay visible.
    if parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    let argv0 = parts.first().map(PathBuf::from);
    let head = (!parts.is_empty()).then(|| redact_argv(&parts).chars().take(120).collect());
    (argv0, head)
}

enum Probe {
    Missing,
    Denied,
    Ok(Option<Orphan>),
}

/// Everything decidable about one pid; races collapse to Missing.
fn probe_pid(pid_dir: &Path, pid: u32, uptime: Option<f64>, scan: &Scan) -> Probe {
    let cwd = std::fs::read_link(pid_dir.join("cwd"));
    let exe = std::fs::read_link(pid_dir.join("exe"));
    if !pid_dir.exists() {
        return Probe::Missing;
    }
    if cwd
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        && exe
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
    {
        return Probe::Denied;
    }
    let mut reasons = Vec::new();
    for target in [cwd.as_deref().ok(), exe.as_deref().ok()]
        .into_iter()
        .flatten()
    {
        if deleted_worktree(target).is_some() {
            reasons.push("cwd/exe under a deleted .cadence/wt worktree");
            break;
        }
    }
    let age = pid_age_secs(pid_dir, uptime);
    let (argv0, head) = cmdline(pid_dir);
    let test_path = exe.as_deref().ok().or(argv0.as_deref());
    if test_path.is_some_and(is_test_binary)
        && age.is_some_and(|a| a >= scan.thresholds.orphan_min_age_secs)
    {
        reasons.push("cargo test binary older than an hour");
    }
    if reasons.is_empty() {
        return Probe::Ok(None);
    }
    Probe::Ok(Some(Orphan {
        pid,
        age_secs: age,
        head: head
            .or_else(|| exe.ok().map(|e| redact_argv(&[e.display().to_string()])))
            .unwrap_or_else(|| "(unknown)".to_string()),
        reasons,
    }))
}

fn check_orphans(scan: &Scan) -> Check {
    let name = "orphans";
    let threshold = json!(format!(
        "warn: any process under a deleted .cadence/wt, or a test binary older than {}s",
        scan.thresholds.orphan_min_age_secs
    ));
    let mut orphans = Vec::new();
    let mut denied = 0_u64;
    let mut vanished = 0_u64;
    let uptime = proc_uptime(&scan.proc_root);
    if let Ok(pids) = std::fs::read_dir(&scan.proc_root) {
        for ent in pids.flatten() {
            let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            match probe_pid(&ent.path(), pid, uptime, scan) {
                Probe::Ok(Some(o)) => orphans.push(o),
                Probe::Denied => denied += 1,
                Probe::Missing => vanished += 1,
                Probe::Ok(None) => {}
            }
        }
    }
    // read_dir order is arbitrary — sort so the pids[] array and the
    // `kill …` remedy are deterministic for users too.
    orphans.sort_by_key(|o| o.pid);
    let level = if orphans.is_empty() {
        Level::Ok
    } else {
        Level::Warn
    };
    let mut detail = if orphans.is_empty() {
        "none".to_string()
    } else {
        format!(
            "{}: {}",
            orphans.len(),
            orphans
                .iter()
                .take(5)
                .map(|o| {
                    let age = o
                        .age_secs
                        .map(|a| format!("{}h", a / 3600))
                        .unwrap_or_else(|| "?h".to_string());
                    format!("pid {} ({} {})", o.pid, age, o.head)
                })
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    if denied + vanished > 0 {
        detail.push_str(&format!(
            "; {} unreadable, {} vanished mid-scan",
            denied, vanished
        ));
    }
    let remedy = if orphans.is_empty() {
        String::new()
    } else {
        format!(
            "kill {}  # orphaned; their worktrees are gone — the watchdog never signals them itself",
            orphans
                .iter()
                .take(10)
                .map(|o| o.pid.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let value = json!({
        "count": orphans.len(),
        "pids": orphans.iter().map(|o| json!({
            "pid": o.pid,
            "age_secs": o.age_secs,
            "head": o.head,
            "reasons": o.reasons,
        })).collect::<Vec<_>>(),
        "unreadable": denied,
        "vanished": vanished,
    });
    check(name, level, value, threshold, detail, remedy)
}

// ---------- leaked temp dirs ----------

fn check_temp_dirs(scan: &Scan) -> Check {
    let name = "temp-dirs";
    let t = &scan.thresholds;
    let threshold = json!(format!(
        "warn: ≥{} dirs older than {}s, or ≥{} total",
        t.temp_warn_count,
        t.temp_min_age_secs,
        human(t.temp_warn_bytes)
    ));
    let mut hits: Vec<(PathBuf, u64)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&scan.temp_dir) {
        for ent in entries.flatten() {
            let name_s = ent.file_name().to_string_lossy().to_string();
            if !TEMP_PREFIXES.iter().any(|p| name_s.starts_with(p)) {
                continue;
            }
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            // Only our own dirs — /tmp is shared, and the remedy prints
            // `rm -rf` for whatever makes the list.
            if meta.uid() != scan.uid {
                continue;
            }
            // An unreadable or future mtime (dir created mid-scan) is
            // treated as fresh — age 0 — never as a leak.
            let age = meta
                .modified()
                .ok()
                .and_then(|m| scan.now.duration_since(m).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if age < t.temp_min_age_secs {
                continue;
            }
            hits.push((ent.path(), 0));
        }
    }
    // Deterministic order with the cadence-specific leaks first —
    // read_dir order would shuffle the remedy line every run.
    hits.sort_by(|a, b| {
        let cad = |p: &PathBuf| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("cadence-"))
        };
        cad(&b.0).cmp(&cad(&a.0)).then(a.0.cmp(&b.0))
    });
    let mut total = 0;
    let mut truncated = false;
    for (path, bytes) in &mut hits {
        let (b, tr) = dir_size(path);
        *bytes = b;
        total += b;
        truncated |= tr;
    }
    let level = if hits.len() as u64 >= t.temp_warn_count || total >= t.temp_warn_bytes {
        Level::Warn
    } else {
        Level::Ok
    };
    let detail = if hits.is_empty() {
        "none".to_string()
    } else if truncated {
        format!("{} dirs, at least {} total", hits.len(), human(total))
    } else {
        format!("{} dirs, {} total", hits.len(), human(total))
    };
    let remedy = if hits.is_empty() {
        String::new()
    } else {
        format!(
            "rm -rf {}  # leaked test/state dirs older than a day{}",
            hits.iter()
                .take(5)
                .map(|(p, _)| shell_quote(&p.display().to_string()))
                .collect::<Vec<_>>()
                .join(" "),
            if hits.len() > 5 {
                format!(" ({} more not listed)", hits.len() - 5)
            } else {
                String::new()
            }
        )
    };
    let value = json!({
        "count": hits.len(),
        "bytes": total,
        "dirs": hits.iter().map(|(p, b)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
    });
    check(name, level, value, threshold, detail, remedy)
}

// ---------- stale worktrees ----------

/// `git worktree list --porcelain` → worktree path to branch name.
fn worktree_branches(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut map = BTreeMap::new();
    let Some(text) = git_stdout(root, &["worktree", "list", "--porcelain"]) else {
        return map;
    };
    let mut current: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            if let Some(path) = current.take() {
                map.insert(path, branch.to_string());
            }
        }
    }
    map
}

/// `origin/HEAD` when set, else a local main/master that verifies.
fn default_base(root: &Path) -> Option<String> {
    if let Some(head) = git_stdout(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    ) {
        return Some(head);
    }
    for cand in ["main", "master"] {
        if git_out(root, &["rev-parse", "--verify", "--quiet", cand])
            .is_some_and(|o| o.status.success())
        {
            return Some(cand.to_string());
        }
    }
    None
}

/// `cad-72-host-watchdog` → `CAD-72`; names without a `<prefix>-<num>`
/// head are not issues.
fn issue_id_from_name(name: &str) -> Option<String> {
    let mut parts = name.split('-');
    let prefix = parts.next()?;
    let num = parts.next()?;
    let ok = !prefix.is_empty()
        && prefix.chars().all(|c| c.is_ascii_lowercase())
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit());
    ok.then(|| format!("{}-{}", prefix.to_uppercase(), num))
}

/// `<pm>/<project>/<ID>/issue.md` — projects are the top-level dirs.
fn find_issue(pm_dir: &Path, id: &str) -> Option<PathBuf> {
    for ent in std::fs::read_dir(pm_dir).ok()?.flatten() {
        let file = ent.path().join(id).join("issue.md");
        if file.is_file() {
            return Some(file);
        }
    }
    None
}

/// Is this worktree's issue closed? `Some(true)` provably closed
/// (status done/dropped, or the worktree ref marked `closed: true`),
/// `Some(false)` still open, `None` no tracker truth available.
fn tracker_closed(scan: &Scan, wt_path: &Path, id: Option<&str>) -> Option<bool> {
    let pm = scan.pm_dir.as_deref()?;
    let file = find_issue(pm, id?)?;
    let text = std::fs::read_to_string(&file).ok()?;
    let (front, _body) = crate::issue::parse::parse_issue(&text).ok()?;
    if matches!(front.status.as_str(), "done" | "dropped") {
        return Some(true);
    }
    let wt = wt_path.to_string_lossy();
    Some(front.refs.iter().any(|r| {
        r.kind == "worktree" && r.closed == Some(true) && r.path.as_deref() == Some(wt.as_ref())
    }))
}

/// The worktree staleness scan shared by the `worktrees` check and
/// `--reclaim-plan`: `(<stale rows>, <remedy per row>, <dirs scanned>)`.
fn stale_worktrees(scan: &Scan, root: &Path, wt_root: &Path) -> (Vec<Value>, Vec<String>, usize) {
    let branches = worktree_branches(root);
    let base = default_base(root);
    let mut stale: Vec<Value> = Vec::new();
    let mut remedies: Vec<String> = Vec::new();
    let mut scanned = 0_usize;
    if let Ok(entries) = std::fs::read_dir(wt_root) {
        for ent in entries.flatten() {
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            scanned += 1;
            let path = ent.path();
            let wt_name = ent.file_name().to_string_lossy().to_string();
            let branch = branches.get(&path).cloned();
            // A merged branch is only stale when the tree is clean —
            // a branch at its base commit is a trivial ancestor, so
            // "merged" alone would flag worktrees whose uncommitted
            // work is still in flight (including the one this runs in).
            let merged = branch
                .as_deref()
                .zip(base.as_deref())
                .is_some_and(|(b, base)| {
                    git_out(root, &["merge-base", "--is-ancestor", b, base])
                        .is_some_and(|o| o.status.success())
                });
            let id = issue_id_from_name(&wt_name);
            let closed = tracker_closed(scan, &path, id.as_deref());
            if !merged && closed != Some(true) {
                continue;
            }
            let clean = git_out(&path, &["status", "--porcelain"])
                .is_some_and(|o| o.status.success() && o.stdout.is_empty());
            let is_stale = closed == Some(true) || (merged && clean);
            if !is_stale {
                continue;
            }
            let (bytes, truncated) = dir_size(&path);
            let mut why = Vec::new();
            if merged {
                why.push(format!("merged into {}", base.as_deref().unwrap_or("?")));
            }
            if closed == Some(true) {
                why.push("tracker ref closed".to_string());
            }
            if !clean {
                why.push("dirty tree".to_string());
            }
            if branch.is_none() {
                why.push("no branch recorded".to_string());
            }
            stale.push(json!({
                "path": path,
                "branch": branch,
                "issue": id,
                "merged": merged,
                "tracker_closed": closed,
                "clean": clean,
                "bytes": bytes,
                "bytes_truncated": truncated,
                "why": why.join(", "),
            }));
            remedies.push(match &id {
                Some(id) => format!("cadence issue finish {id}"),
                None => format!(
                    "git -C {} worktree remove {}",
                    shell_quote(&root.display().to_string()),
                    shell_quote(&path.display().to_string())
                ),
            });
        }
    }
    (stale, remedies, scanned)
}

fn check_worktrees(scan: &Scan) -> Check {
    let name = "worktrees";
    let threshold =
        json!("warn: any worktree whose branch is merged or whose tracker ref is closed");
    let Some(root) = repo_root(&scan.cwd) else {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            format!("{} is not inside a git repo", scan.cwd.display()),
            String::new(),
        );
    };
    let wt_root = root.join(".cadence/wt");
    if !wt_root.is_dir() {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            format!("no .cadence/wt under {}", root.display()),
            String::new(),
        );
    }
    let (stale, remedies, scanned) = stale_worktrees(scan, &root, &wt_root);
    // The shared cargo cache counts once, at the repo level — it is
    // not part of any worktree's own footprint.
    let shared = crate::worktree::shared_target_dir(&root);
    let shared_size = shared.is_dir().then(|| {
        let (bytes, truncated) = dir_size(&shared);
        json!({"path": shared, "bytes": bytes, "bytes_truncated": truncated})
    });
    let level = if stale.is_empty() {
        Level::Ok
    } else {
        Level::Warn
    };
    let shared_note = shared_size
        .as_ref()
        .map(|s| {
            format!(
                "; shared cargo cache {}{}",
                if s["bytes_truncated"].as_bool().unwrap_or(false) {
                    "at least "
                } else {
                    ""
                },
                human(s["bytes"].as_u64().unwrap_or(0))
            )
        })
        .unwrap_or_default();
    let detail = if stale.is_empty() {
        format!("{scanned} worktrees, none stale{shared_note}")
    } else {
        format!(
            "{} of {} worktrees stale ({}{}{})",
            stale.len(),
            scanned,
            if stale
                .iter()
                .any(|s| s["bytes_truncated"].as_bool().unwrap_or(false))
            {
                "at least "
            } else {
                ""
            },
            human(stale.iter().map(|s| s["bytes"].as_u64().unwrap_or(0)).sum()),
            shared_note
        )
    };
    check(
        name,
        level,
        json!({
            "scanned": scanned,
            "stale": stale,
            "shared_cargo_target": shared_size,
        }),
        threshold,
        detail,
        remedies.into_iter().take(4).collect::<Vec<_>>().join("; "),
    )
}

// ---------- reclaim plan ----------

/// What `--reclaim-plan` lists: live lanes' `target/` dirs
/// (informational — they free only when the lane does), the shared
/// cache's reclaimable subdirs (cleared contents-only, and only when
/// no build holds one of cargo's lock files), retired shared dirs an
/// older cadence planted, and stale worktrees. The stale scan runs
/// first: a stale lane's *whole* dir — `target/` included — is freed
/// by that row's own `issue finish`/`worktree remove` command, so its
/// bytes count toward `reclaimable_bytes` and it gets no separate
/// informational row. Live-lane `target/` rows report separately as
/// `freed_with_lanes_bytes` — the headline number is what the plan's
/// own commands free today. A locked shared cache emits no freeing
/// command, so its bytes stay out of the total too. Listing only:
/// nothing here deletes or signals anything, and every emitted
/// command is shell-quoted so a path with a space can never split
/// into extra `rm -rf` arguments.
pub fn reclaim_plan(scan: &Scan) -> Value {
    use crate::worktree::{RETIRED_DEBUG_DIRS, SHARED_DEBUG_DIRS, SHARED_DEBUG_FILES};
    let mut rows: Vec<Value> = Vec::new();
    let Some(root) = repo_root(&scan.cwd) else {
        return json!({"rows": rows, "reclaimable_bytes": 0, "freed_with_lanes_bytes": 0,
                      "skipped": format!("{} is not inside a git repo", scan.cwd.display())});
    };
    let wt_root = root.join(".cadence/wt");
    // Stale scan first — a stale lane's whole dir is freed by its own
    // row's command, so it must not also emit an informational
    // worktree-target row.
    let mut stale_rows: Vec<Value> = Vec::new();
    let mut stale_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if wt_root.is_dir() {
        let (stale, _remedies, _scanned) = stale_worktrees(scan, &root, &wt_root);
        for s in stale {
            let path = PathBuf::from(s["path"].as_str().unwrap_or_default());
            stale_paths.insert(path.clone());
            stale_rows.push(json!({
                "kind": "stale-worktree",
                "path": s["path"],
                "bytes": s["bytes"],
                "bytes_truncated": s["bytes_truncated"],
                "filesystem": fs_label(&path),
                "action": match s["issue"].as_str() {
                    Some(id) => format!("cadence issue finish {id}"),
                    None => format!(
                        "git -C {} worktree remove {}",
                        shell_quote(&root.display().to_string()),
                        shell_quote(s["path"].as_str().unwrap_or("?"))
                    ),
                },
                "why": s["why"],
            }));
        }
    }
    // Live lanes' target/ rows — informational, freed with the lane.
    if let Ok(entries) = std::fs::read_dir(&wt_root) {
        for ent in entries.flatten() {
            if !ent.metadata().is_ok_and(|m| m.is_dir()) || stale_paths.contains(&ent.path()) {
                continue;
            }
            let target = ent.path().join("target");
            if target.is_dir() {
                let (bytes, truncated) = dir_size(&target);
                let name = ent.file_name().to_string_lossy().to_string();
                rows.push(json!({
                    "kind": "worktree-target",
                    "path": target,
                    "bytes": bytes,
                    "bytes_truncated": truncated,
                    "filesystem": fs_label(&target),
                    "action": match issue_id_from_name(&name) {
                        // "Freed with the lane" — a live lane's target
                        // is not a recommendation to finish its work.
                        Some(id) => format!("freed with the lane — cadence issue finish {id} removes it"),
                        None => format!(
                            "freed with the lane — git -C {} worktree remove {}",
                            shell_quote(&root.display().to_string()),
                            shell_quote(&ent.path().display().to_string())
                        ),
                    },
                }));
            }
        }
    }
    let shared = crate::worktree::shared_target_dir(&root);
    if shared.is_dir() {
        let d = shared.join("debug");
        // `bytes` counts exactly what the emitted command frees: the
        // contents of the shared subdirs — nothing else in the tree.
        let mut bytes = 0_u64;
        let mut truncated = false;
        for name in SHARED_DEBUG_DIRS {
            let (b, t) = dir_size(&d.join(name));
            bytes += b;
            truncated |= t;
        }
        // Any of cargo's lock files held means a build is live.
        let locked = SHARED_DEBUG_FILES
            .iter()
            .any(|name| file_locked(&d.join(name)));
        rows.push(json!({
            "kind": "shared-cargo-cache",
            "path": shared,
            "bytes": bytes,
            "bytes_truncated": truncated,
            "filesystem": fs_label(&shared),
            "cargo_locked": locked,
            "action": if locked {
                // A snapshot flag — the lock may already be free by
                // the time anyone reads this; the emitted command is
                // withheld rather than risking a live build.
                "a cargo build held the shared lock at scan time — \
                 rerun the plan when lanes are idle".to_string()
            } else {
                // Clear the *contents* of the hashed subdirs, never
                // the dirs themselves: every lane symlinks to those
                // dirs, and deleting them would leave the links
                // dangling — cargo dies with EEXIST on the next
                // build. `*` plus `.[!.]*` covers dotfiles too; on an
                // empty dir both go unmatched and `-f` swallows the
                // literal argument.
                let globs = SHARED_DEBUG_DIRS
                    .iter()
                    .map(|name| {
                        let q = shell_quote(&d.join(name).display().to_string());
                        format!("{q}/* {q}/.[!.]*")
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                format!(
                    "rm -rf {globs}  # clears cached dep artifacts — \
                     the dirs stay, so lanes keep building and rebuild lazily"
                )
            },
        }));
        // Dirs an older cadence shared but none does now — the
        // contents clear the same way; the dir itself stays for any
        // lane whose r2-era link still points at it.
        for name in RETIRED_DEBUG_DIRS {
            let dir = d.join(name);
            if dir.is_dir() && !dir.is_symlink() {
                let (b, t) = dir_size(&dir);
                let q = shell_quote(&dir.display().to_string());
                rows.push(json!({
                    "kind": "retired-shared-dir",
                    "path": dir,
                    "bytes": b,
                    "bytes_truncated": t,
                    "filesystem": fs_label(&dir),
                    "action": format!(
                        "rm -rf {q}/* {q}/.[!.]*  # retired shared dir no current lane links"
                    ),
                }));
            }
        }
    }
    rows.extend(stale_rows);
    // `reclaimable_bytes` = what the emitted commands free today:
    // every row except live-lane targets (freed with their lane) and
    // a lock-blocked shared cache (no command emitted).
    let reclaimable: u64 = rows
        .iter()
        .filter(|r| r["kind"] != "worktree-target")
        .filter(|r| !(r["kind"] == "shared-cargo-cache" && r["cargo_locked"] == json!(true)))
        .map(|r| r["bytes"].as_u64().unwrap_or(0))
        .sum();
    let with_lanes: u64 = rows
        .iter()
        .filter(|r| r["kind"] == "worktree-target")
        .map(|r| r["bytes"].as_u64().unwrap_or(0))
        .sum();
    json!({
        "rows": rows,
        "reclaimable_bytes": reclaimable,
        "freed_with_lanes_bytes": with_lanes,
    })
}

/// Text form of the plan: one line per reclaimable row, then the total.
pub fn render_reclaim(plan: &Value) -> String {
    let mut out = String::from("cadence doctor --host --reclaim-plan — nothing here is deleted\n");
    if let Some(skipped) = plan["skipped"].as_str() {
        out.push_str(&format!("skipped: {skipped}\n"));
        return out;
    }
    if let Some(rows) = plan["rows"].as_array() {
        for r in rows {
            let size = if r["bytes_truncated"].as_bool().unwrap_or(false) {
                format!("≥{}", human(r["bytes"].as_u64().unwrap_or(0)))
            } else {
                human(r["bytes"].as_u64().unwrap_or(0))
            };
            let fs = r["filesystem"]
                .as_str()
                .map(|f| format!(" on {f}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "{:<20} {:>10}  {}{}\n       {}\n",
                r["kind"].as_str().unwrap_or("?"),
                size,
                r["path"].as_str().unwrap_or("?"),
                fs,
                r["action"].as_str().unwrap_or("")
            ));
        }
    }
    out.push_str(&format!(
        "total reclaimable: {}\n",
        human(plan["reclaimable_bytes"].as_u64().unwrap_or(0))
    ));
    let with_lanes = plan["freed_with_lanes_bytes"].as_u64().unwrap_or(0);
    if with_lanes > 0 {
        out.push_str(&format!(
            "target/ freed with their lanes: {} (not counted above)\n",
            human(with_lanes)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use tempfile::TempDir;

    /// A fully fabricated host: empty proc/tmp/home/repo/pm under one
    /// temp root. Tests poke in exactly the state a check reads.
    fn fake_scan(root: &TempDir) -> Scan {
        let scan = Scan {
            proc_root: root.path().join("proc"),
            temp_dir: root.path().join("tmp"),
            home: root.path().join("home"),
            state_dir: root.path().join("state"),
            cwd: root.path().join("repo"),
            devin_data: root.path().join("devin"),
            claude_projects: root.path().join("claude-projects"),
            codex_sessions: root.path().join("codex-sessions"),
            pm_dir: Some(root.path().join("pm")),
            uid: unsafe { libc::geteuid() },
            now: SystemTime::now(),
            thresholds: Thresholds::default(),
            linux: true,
            fs_probe: Some(|path| {
                Some(FsFree {
                    path: path.to_path_buf(),
                    dev: 1,
                    free: 500 * GIB,
                    total: 1_000 * GIB,
                })
            }),
            census: std::cell::OnceCell::new(),
        };
        for d in [
            &scan.proc_root,
            &scan.temp_dir,
            &scan.home,
            &scan.state_dir,
            &scan.cwd,
            scan.pm_dir.as_ref().unwrap(),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        scan
    }

    /// proc/<pid>/ with the files the checks read: stat (age), cmdline,
    /// cwd/exe symlinks, fd dir with the given link targets.
    fn add_pid(
        proc: &Path,
        pid: u32,
        cwd: Option<&Path>,
        exe: Option<&Path>,
        cmdline: Option<&str>,
        age_secs: u64,
        fds: &[&str],
    ) -> PathBuf {
        // Space-joined form for callers whose args are space-free;
        // `add_pid_argv` takes the real argv vector.
        add_pid_argv(
            proc,
            pid,
            cwd,
            exe,
            cmdline.map(|c| c.split(' ').collect::<Vec<_>>()).as_deref(),
            age_secs,
            fds,
        )
    }

    /// `add_pid` with a real argv vector — an element may itself
    /// contain spaces (a `-H "Name: value"` header).
    fn add_pid_argv(
        proc: &Path,
        pid: u32,
        cwd: Option<&Path>,
        exe: Option<&Path>,
        argv: Option<&[&str]>,
        age_secs: u64,
        fds: &[&str],
    ) -> PathBuf {
        let dir = proc.join(pid.to_string());
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        // uptime file says 1_000_000s; starttime = uptime - age in
        // jiffies (field 22 → token 19 after the comm paren).
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        let starttime = (1_000_000_u64.saturating_sub(age_secs)) * hz;
        std::fs::write(
            dir.join("stat"),
            format!("{pid} (t) S {}{starttime} 0 0", "1 ".repeat(18)),
        )
        .unwrap();
        std::fs::write(proc.join("uptime"), "1000000.00 0.00\n").unwrap();
        if let Some(args) = argv {
            std::fs::write(dir.join("cmdline"), args.join("\0")).unwrap();
        }
        if let Some(cwd) = cwd {
            std::os::unix::fs::symlink(cwd, dir.join("cwd")).unwrap();
        }
        if let Some(exe) = exe {
            std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
        }
        for (i, target) in fds.iter().enumerate() {
            std::os::unix::fs::symlink(target, dir.join("fd").join((i + 3).to_string())).unwrap();
        }
        dir
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(root: &Path) {
        git(root, &["init", "-q", "-b", "main", "."]);
        // A real repo ignores build output — `target/` must not read
        // as dirty.
        std::fs::write(root.join(".gitignore"), "/target\n").unwrap();
        git(root, &["add", "-A"]);
        git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "init",
            ],
        );
    }

    fn write_issue(pm: &Path, project: &str, id: &str, status: &str, refs_yaml: &str) {
        let dir = pm.join(project).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("issue.md"),
            format!(
                "---\nid: {id}\ntitle: t\nstatus: {status}\npriority: P2\n{refs_yaml}created: 2026-09-19T00:00:00Z\n---\n\nbody\n"
            ),
        )
        .unwrap();
    }

    fn sparse(path: &Path, bytes: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::File::create(path).unwrap().set_len(bytes).unwrap();
    }

    /// `dir_size` measures allocated blocks (`du`-style) — a sparse
    /// file reports ~0 — so tests that need real bytes write them.
    fn real_bytes(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; bytes]).unwrap();
    }

    fn set_mtime_old(path: &Path, secs_ago: i64) {
        let c = CString::new(path.as_os_str().as_bytes()).unwrap();
        let when = libc::timespec {
            tv_sec: (SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                - secs_ago) as libc::time_t,
            tv_nsec: 0,
        };
        let times = [when, when];
        unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
    }

    // ---------- disk ----------

    #[test]
    fn disk_level_threshold_edges() {
        let t = Thresholds::default();
        let gib = |n: u64| n * GIB;
        // Plenty free.
        assert_eq!(fs_level(gib(50), gib(100), &t), Level::Ok);
        // 14% free < 15% warn.
        assert_eq!(fs_level(gib(14), gib(100), &t), Level::Warn);
        // Exactly 15% with enough bytes is ok.
        assert_eq!(fs_level(gib(30), gib(200), &t), Level::Ok);
        // Percent fine but bytes under 20 GiB warn.
        assert_eq!(fs_level(gib(19), gib(200), &t), Level::Warn);
        // Under 5% fails.
        assert_eq!(fs_level(gib(4), gib(100), &t), Level::Fail);
        // Exactly 5% and 5 GiB stays warn, not fail.
        assert_eq!(fs_level(gib(5), gib(100), &t), Level::Warn);
        // Percent fine but bytes under 5 GiB fails.
        assert_eq!(fs_level(gib(4), gib(400), &t), Level::Fail);
    }

    #[test]
    fn disk_check_reports_each_filesystem() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let c = check_disk(&scan);
        // Same fs under the temp root dedups to one filesystem entry.
        let fses = c.value.as_array().unwrap();
        assert!(!fses.is_empty());
        assert_eq!(c.name, "disk");
        assert!(c.detail.contains("% free"));
    }

    // ---------- provider state ----------

    #[test]
    fn provider_state_threshold_edges() {
        let t = Thresholds::default();
        assert_eq!(store_level(0, Some(GIB + 1), &t), Level::Warn);
        assert_eq!(store_level(0, Some(10 * GIB + 1), &t), Level::Fail);
        assert_eq!(store_level(10 * GIB + 1, None, &t), Level::Warn);
        assert_eq!(store_level(9 * GIB, Some(MIB), &t), Level::Ok);
        assert_eq!(store_level(0, Some(GIB), &t), Level::Ok); // exactly at warn stays ok
    }

    #[test]
    fn provider_state_missing_dirs_skip_not_fail() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let c = check_provider_state(&scan);
        assert_eq!(c.level, Level::Ok);
        assert!(c.detail.contains("no known provider stores"));
    }

    #[test]
    fn provider_state_flags_fat_wal_and_store() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        // devin sessions.db + a WAL past the fail line (sparse files).
        sparse(&scan.devin_data.join("cli/sessions.db"), 100);
        sparse(&scan.devin_data.join("cli/sessions.db-wal"), 11 * GIB);
        // a directory store over the warn line.
        scan.claude_projects = root.path().join("claude-fat");
        sparse(&scan.claude_projects.join("blob"), 11 * GIB);
        let c = check_provider_state(&scan);
        assert_eq!(c.level, Level::Fail);
        assert!(c.remedy.contains("wal_checkpoint(TRUNCATE)"));
        let stores = c.value.as_array().unwrap();
        assert_eq!(stores.len(), 2); // absent stores skipped
    }

    // ---------- memory + census ----------

    /// Write a fabricated `/proc/meminfo` (values in kB, like the real
    /// file) plus the overcommit sysctl.
    fn write_meminfo(scan: &Scan, meminfo: &str, overcommit: Option<u64>) {
        std::fs::write(scan.proc_root.join("meminfo"), meminfo).unwrap();
        if let Some(mode) = overcommit {
            std::fs::create_dir_all(scan.proc_root.join("sys/vm")).unwrap();
            std::fs::write(
                scan.proc_root.join("sys/vm/overcommit_memory"),
                format!("{mode}\n"),
            )
            .unwrap();
        } else {
            let _ = std::fs::remove_file(scan.proc_root.join("sys/vm/overcommit_memory"));
        }
    }

    /// proc/<pid>/stat with a real comm, cpu jiffies and rss pages —
    /// the census's whole input. Age comes from the shared 1e6s uptime.
    fn add_proc(
        proc: &Path,
        pid: u32,
        comm: &str,
        age_secs: u64,
        cpu_secs: u64,
        rss_pages: u64,
    ) -> PathBuf {
        let dir = proc.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        let starttime = (1_000_000_u64.saturating_sub(age_secs)) * hz;
        // Post-comm fields, positions 3..: state S, fields 4-13 zero,
        // utime(14)/stime(15) carry the cpu, fields 16-21 filler,
        // starttime(22), vsize(23)=0, rss(24).
        std::fs::write(
            dir.join("stat"),
            format!(
                "{pid} ({comm}) S 0 0 0 0 0 0 0 0 0 0 {utime} {stime} 0 0 0 1 0 0 {starttime} 0 {rss_pages}",
                utime = cpu_secs * hz,
                stime = 0,
            ),
        )
        .unwrap();
        std::fs::write(proc.join("uptime"), "1000000.00 0.00\n").unwrap();
        dir
    }

    /// The measured CAD-154 incident: 3.6 GiB available of ~32 GiB,
    /// 208 MiB of 20 GiB swap free, Committed_AS ~99.9 GiB against a
    /// ~35.4 GiB CommitLimit with heuristic overcommit — fork() was
    /// already returning EAGAIN while every check was green.
    #[test]
    fn memory_incident_numbers_fail() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        write_meminfo(
            &scan,
            "MemTotal:       33554432 kB\n\
             MemAvailable:    3774873 kB\n\
             SwapTotal:      20971520 kB\n\
             SwapFree:         212992 kB\n\
             Committed_AS:  104752742 kB\n\
             CommitLimit:    37119590 kB\n",
            Some(0),
        );
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Fail, "{}", c.detail);
        assert!(
            c.detail.contains("overcommit_memory=heuristic"),
            "{}",
            c.detail
        );
        // The fail comes from exhausted swap *with* low available —
        // the commit overshoot is an advisory item under mode 0.
        assert!(c.value["committed_over_limit"].as_bool().unwrap());
        assert!(c.remedy.contains("cadence never kills"), "{}", c.remedy);
    }

    #[test]
    fn memory_threshold_edges() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let kb = |gib: u64| gib * 1024 * 1024;
        // Exactly at warn (15%) with swap comfy → ok.
        write_meminfo(
            &scan,
            &format!(
                "MemTotal: {t} kB\nMemAvailable: {} kB\nSwapTotal: {s} kB\nSwapFree: {} kB\n",
                kb(15) + kb(15) / 100 + 1024,
                kb(10),
                t = kb(100),
                s = kb(20)
            ),
            Some(0),
        );
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Ok, "{}", c.detail);
        // 14% available → warn; no commit fields → commit leg silent.
        write_meminfo(
            &scan,
            &format!(
                "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
                kb(100),
                kb(14),
                kb(20),
                kb(10)
            ),
            Some(0),
        );
        assert_eq!(check_memory(&scan).level, Level::Warn);
        // 4% available → fail.
        write_meminfo(
            &scan,
            &format!(
                "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
                kb(100),
                kb(4),
                kb(20),
                kb(10)
            ),
            Some(0),
        );
        assert_eq!(check_memory(&scan).level, Level::Fail);
        // Swap free 15% (<20 warn) → warn. Swap at 4% with RAM to
        // spare is still only warn — swap exhaustion alone is the
        // steady state of a long-lived host, not a failure.
        write_meminfo(
            &scan,
            &format!(
                "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
                kb(100),
                kb(90),
                kb(20),
                kb(3)
            ),
            Some(0),
        );
        assert_eq!(check_memory(&scan).level, Level::Warn);
        write_meminfo(
            &scan,
            &format!(
                "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
                kb(100),
                kb(90),
                kb(20),
                kb(20) / 25
            ),
            Some(0),
        );
        assert_eq!(check_memory(&scan).level, Level::Warn);
        // Swap exhausted AND available low together — the incident
        // shape — is the fail.
        write_meminfo(
            &scan,
            &format!(
                "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
                kb(100),
                kb(10),
                kb(20),
                kb(20) / 25
            ),
            Some(0),
        );
        assert_eq!(check_memory(&scan).level, Level::Fail);
    }

    #[test]
    fn memory_commit_leg_modes() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        // Comfy memory, commitment over the limit — the mode decides.
        let base = "MemTotal: 104857600 kB\nMemAvailable: 83886080 kB\n\
                    SwapTotal: 20971520 kB\nSwapFree: 20971520 kB\n\
                    Committed_AS: 60000000 kB\nCommitLimit: 40000000 kB\n";
        // overcommit_memory=1 (always): over-limit is benign — no fail.
        write_meminfo(&scan, base, Some(1));
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Ok, "{}", c.detail);
        assert!(c.detail.contains("overcommit_memory=always"));
        // overcommit_memory=2 (strict): refusing allocations now.
        write_meminfo(&scan, base, Some(2));
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Fail, "{}", c.detail);
        assert!(c.detail.contains("strict overcommit"), "{}", c.detail);
        // overcommit_memory=0 (heuristic): CommitLimit is advisory —
        // over-limit is a normal steady state, reported not alarmed.
        write_meminfo(&scan, base, Some(0));
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Ok, "{}", c.detail);
        assert!(c.detail.contains("overcommit_memory=heuristic"));
        assert!(c.value["committed_over_limit"].as_bool().unwrap());
        // Sysctl unreadable: cannot prove enforcement — warn, not fail.
        write_meminfo(&scan, base, None);
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Warn, "{}", c.detail);
        assert!(c.detail.contains("overcommit_memory=unknown"));
        // Under the limit: quiet regardless of mode.
        write_meminfo(
            &scan,
            "MemTotal: 104857600 kB\nMemAvailable: 83886080 kB\n\
             SwapTotal: 20971520 kB\nSwapFree: 20971520 kB\n\
             Committed_AS: 10000000 kB\nCommitLimit: 40000000 kB\n",
            Some(2),
        );
        assert_eq!(check_memory(&scan).level, Level::Ok);
    }

    #[test]
    fn memory_no_swap_and_missing_meminfo() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        // No meminfo at all → skipped, not a false fail.
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Ok);
        assert!(c.value["skipped"].as_bool().unwrap());
        // A host with no swap: the leg reports "no swap", no fail.
        write_meminfo(
            &scan,
            "MemTotal: 104857600 kB\nMemAvailable: 83886080 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
            Some(0),
        );
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Ok, "{}", c.detail);
        assert!(c.detail.contains("no swap"), "{}", c.detail);
        // Off-linux the whole check skips.
        let mut scan = fake_scan(&root);
        scan.linux = false;
        assert!(check_memory(&scan).value["skipped"].as_bool().unwrap());
    }

    #[test]
    fn memory_remedy_names_biggest_groups() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        write_meminfo(
            &scan,
            "MemTotal: 33554432 kB\nMemAvailable: 1572864 kB\n",
            Some(0),
        );
        // Leaked browser tree: 3 chrome pids, 50h old, ~0 cpu.
        for pid in [11, 12, 13] {
            add_proc(&scan.proc_root, pid, "chrome", 180_000, 0, 200_000);
        }
        add_proc(&scan.proc_root, 20, "node", 60, 30, 10_000);
        let c = check_memory(&scan);
        assert_eq!(c.level, Level::Fail);
        assert!(c.remedy.contains("chrome ×3"), "{}", c.remedy);
        assert!(c.remedy.contains("oldest 50h"), "{}", c.remedy);
        assert!(c.remedy.contains("idle"), "{}", c.remedy);
        // Remedies name offenders; cadence never kills.
        assert!(c.remedy.contains("cadence never kills"), "{}", c.remedy);
    }

    #[test]
    fn census_groups_and_oldest_idle() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        // Leaked tree: root-owned old chrome at ~zero cpu, plus a busy
        // node and a young claude.
        let euid = unsafe { libc::geteuid() };
        for (pid, comm) in [(11, "chrome"), (12, "chrome_crashpad"), (13, "chrome")] {
            add_proc(&scan.proc_root, pid, comm, 180_000, 0, 100_000);
        }
        add_proc(&scan.proc_root, 20, "node", 120, 90, 50_000);
        add_proc(&scan.proc_root, 30, "claude", 30, 5, 20_000);
        let census = proc_census(&scan);
        assert_eq!(census.procs, 5);
        let chrome = &census.groups["chrome"];
        assert_eq!(chrome.count, 3);
        assert!(chrome.uids.contains(&euid));
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        assert_eq!(chrome.rss_bytes, 3 * 100_000 * page);
        let oldest = chrome.oldest_idle.as_ref().unwrap();
        assert_eq!(oldest.age_secs, 180_000);
        assert!(oldest.idle);
        // node burned 90 cpu-seconds in 120 — busy, not idle.
        assert!(census.groups["node"].oldest_idle.is_none());
        assert_eq!(census.groups["node"].oldest.as_ref().unwrap().age_secs, 120);
        let c = check_processes(&scan);
        assert_eq!(c.level, Level::Ok);
        assert!(c.detail.contains("chrome ×3"), "{}", c.detail);
        assert!(c.detail.contains("50h"), "{}", c.detail);
    }

    #[test]
    fn census_family_normalization() {
        assert_eq!(comm_family("chrome_crashpad"), "chrome");
        assert_eq!(comm_family("Chrome"), "chrome");
        assert_eq!(comm_family("node"), "node");
        assert_eq!(comm_family("rust-analyzer"), "rust-analyzer");
        assert_eq!(comm_family("weird-daemon"), "weird-daemon");
    }

    #[test]
    fn census_skipped_off_linux() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        scan.linux = false;
        let c = check_processes(&scan);
        assert!(c.value["skipped"].as_bool().unwrap());
    }

    // ---------- WAL roots ----------

    #[test]
    fn find_wals_walks_provider_roots() {
        let root = TempDir::new().unwrap();
        let codex = root.path().join("codex");
        // devin's store, a codex *.sqlite, a nested claude db.
        let devin_cli = root.path().join("devin/cli");
        std::fs::create_dir_all(&devin_cli).unwrap();
        std::fs::write(devin_cli.join("sessions.db-wal"), "x").unwrap();
        real_bytes(&codex.join("state_1.sqlite-wal"), 8);
        real_bytes(&codex.join("queue_1.db-wal"), 8);
        real_bytes(&root.path().join("claude/proj/sub/sess.sqlite3-wal"), 8);
        // Not a sqlite store — ignored.
        real_bytes(&codex.join("notes.txt-wal"), 8);
        let found = find_wals(root.path());
        assert!(!found.truncated);
        let names: Vec<String> = found
            .dbs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"sessions.db".to_string()), "{names:?}");
        assert!(names.contains(&"state_1.sqlite".to_string()), "{names:?}");
        assert!(names.contains(&"queue_1.db".to_string()), "{names:?}");
        assert!(names.contains(&"sess.sqlite3".to_string()), "{names:?}");
        assert_eq!(found.dbs.len(), 4, "{names:?}");
    }

    #[test]
    fn wal_roots_cover_the_three_providers() {
        let roots = wal_roots(Path::new("/home/u"), Path::new("/data"));
        let providers: Vec<&str> = roots.iter().map(|r| r.provider).collect();
        assert_eq!(providers, ["devin", "codex", "claude"]);
        assert!(roots[0].root.ends_with("devin/cli"));
        assert!(roots[1].root.ends_with(".codex"));
        assert!(roots[2].root.ends_with(".claude/projects"));
    }

    /// A symlinked dir inside a provider root is not descended — the
    /// walk must stay inside the root it was given.
    #[test]
    fn find_wals_skips_symlinked_dirs() {
        let root = TempDir::new().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        real_bytes(&real.join("a.db-wal"), 8);
        std::os::unix::fs::symlink(&real, root.path().join("link")).unwrap();
        let found = find_wals(root.path());
        assert_eq!(
            found.dbs.len(),
            1,
            "the same wal must not be found twice through the link"
        );
    }

    /// Hitting the matched-db cap reports truncation rather than
    /// silently returning a partial watch list.
    #[test]
    fn find_wals_reports_truncation() {
        let root = TempDir::new().unwrap();
        for i in 0..1030 {
            real_bytes(&root.path().join(format!("s{i}.db-wal")), 4);
        }
        let found = find_wals(root.path());
        assert!(found.truncated, "1024-cap must surface");
        assert_eq!(found.dbs.len(), 1024);
    }

    // ---------- pipes ----------

    #[test]
    fn pipes_counts_fifos_and_flags_soft_limit() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        let proc = scan.proc_root.clone();
        std::fs::create_dir_all(proc.join("sys/fs")).unwrap();
        std::fs::write(proc.join("sys/fs/pipe-user-pages-soft"), "32\n").unwrap();
        std::fs::write(proc.join("sys/fs/pipe-max-size"), "1048576\n").unwrap();
        std::fs::write(proc.join("uptime"), "1000.00 0.00\n").unwrap();
        // pid 10: 3 fds on 2 unique pipes + a socket that is not a pipe.
        add_pid(
            &proc,
            10,
            None,
            None,
            Some("holder"),
            60,
            &["pipe:[11]", "pipe:[11]", "pipe:[22]", "socket:[9]"],
        );
        // pid 11: one more pipe.
        add_pid(&proc, 11, None, None, Some("other"), 60, &["pipe:[33]"]);
        // pid 12: unreadable fd dir — counted, not fatal.
        let denied = add_pid(&proc, 12, None, None, Some("denied"), 60, &[]);
        std::fs::set_permissions(denied.join("fd"), std::fs::Permissions::from_mode(0o0)).unwrap();
        // pid 13: fd dir missing entirely — read_dir fails, tolerated.
        std::fs::create_dir_all(proc.join("13")).unwrap();
        // non-pid entries are ignored.
        std::fs::create_dir_all(proc.join("sys")).unwrap();
        let stats = scan_pipes(&scan);
        assert_eq!(stats.fds, 4);
        assert_eq!(stats.pipes, 3);
        assert_eq!(stats.denied, 1);
        assert_eq!(stats.top[0], (10, 3));
        // est_pages = 3 × 16 = 48 > soft 32 → warn + clamped.
        let c = check_pipes(&scan);
        assert_eq!(c.level, Level::Warn);
        assert!(c.value["clamped"].as_bool().unwrap());
        // Now raise the soft limit — the same pipes are fine.
        std::fs::write(proc.join("sys/fs/pipe-user-pages-soft"), "16384\n").unwrap();
        scan.linux = true;
        let c = check_pipes(&scan);
        assert_eq!(c.level, Level::Ok);
        assert!(!c.value["clamped"].as_bool().unwrap());
    }

    #[test]
    fn pipes_skipped_off_linux() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        scan.linux = false;
        let c = check_pipes(&scan);
        assert_eq!(c.level, Level::Ok);
        assert!(c.value["skipped"].as_bool().unwrap());
    }

    // ---------- orphans ----------

    #[test]
    fn orphans_flag_deleted_worktrees_and_old_test_binaries() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let proc = scan.proc_root.clone();
        let repo = scan.cwd.clone();
        // live worktree dir — must NOT flag.
        let alive = repo.join(".cadence/wt/alive");
        std::fs::create_dir_all(&alive).unwrap();
        // pid 20: cwd in a deleted worktree.
        add_pid(
            &proc,
            20,
            Some(&repo.join(".cadence/wt/gone")),
            None,
            Some("sleep 99"),
            60,
            &[],
        );
        // pid 21: cwd in a live worktree — not an orphan.
        add_pid(
            &proc,
            21,
            Some(&alive),
            None,
            Some("cargo test"),
            80_000,
            &[],
        );
        // pid 22: exe is a test binary in a deleted worktree, 3h old.
        add_pid(
            &proc,
            22,
            None,
            Some(&repo.join(".cadence/wt/gone2/target/debug/deps/integration-abc123")),
            Some("integration-abc123"),
            10_800,
            &[],
        );
        // pid 23: test binary outside any worktree, 2h old — flagged
        // on age alone.
        add_pid(
            &proc,
            23,
            None,
            Some(&repo.join("target/debug/deps/board-deadbeef")),
            Some("board-deadbeef"),
            7_200,
            &[],
        );
        // pid 24: same test-binary shape but only 10 min — too young.
        add_pid(
            &proc,
            24,
            None,
            Some(&repo.join("target/debug/deps/board-young")),
            Some("board-young"),
            600,
            &[],
        );
        // pid 25: ordinary long-lived process nowhere near a worktree.
        add_pid(
            &proc,
            25,
            None,
            Some(Path::new("/usr/bin/sleep")),
            Some("sleep 99"),
            90_000,
            &[],
        );
        let c = check_orphans(&scan);
        assert_eq!(c.level, Level::Warn);
        let pids: Vec<u64> = c.value["pids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["pid"].as_u64().unwrap())
            .collect();
        // Sorted by pid regardless of read_dir order.
        assert_eq!(pids, vec![20, 22, 23]);
        assert!(c.remedy.contains("kill 20 22 23"));
        assert!(c.detail.contains("pid 20"));
    }

    #[test]
    fn orphans_tolerate_denied_and_vanished_pids() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let proc = scan.proc_root.clone();
        // Denied: cwd+exe both unreadable inside an existing pid dir.
        let denied = add_pid(&proc, 30, None, None, Some("x"), 1, &[]);
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o0)).unwrap();
        // Vanished: a pid dir that is gone when probed.
        let gone = proc.join("31");
        let probe = probe_pid(&gone, 31, Some(1_000.0), &scan);
        assert!(matches!(probe, Probe::Missing));
        let c = check_orphans(&scan);
        assert_eq!(c.value["unreadable"].as_u64().unwrap(), 1);
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Nothing else flags → ok.
        assert_eq!(c.level, Level::Ok);
    }

    // ---------- argv redaction (CAD-108) ----------

    #[test]
    fn redact_argv_flag_and_env_forms() {
        let cases: Vec<(Vec<&str>, &str)> = vec![
            // `--flag=value`
            (
                vec![
                    "npm",
                    "exec",
                    "figma-developer-mcp",
                    "--figma-api-key=figd_TESTKEY0001",
                    "--stdio",
                ],
                "npm exec figma-developer-mcp --figma-api-key=[REDACTED] --stdio",
            ),
            // `--flag value`
            (
                vec!["tool", "--token", "s3cr3t", "--verbose"],
                "tool --token [REDACTED] --verbose",
            ),
            (
                vec!["aws", "sso", "--aws-secret-access-key", "wJalrXUtnFEMI"],
                "aws sso --aws-secret-access-key [REDACTED]",
            ),
            // a flag-shaped next arg is still the value — the flag
            // names a secret, so `--verbose` is eaten
            (vec!["t", "--token", "--verbose"], "t --token [REDACTED]"),
            // …but a secret flag is never eaten as a value
            (
                vec!["t", "--token", "--password", "hunter2"],
                "t --token --password [REDACTED]",
            ),
            // trailing flag with no value — nothing to redact
            (vec!["t", "--verbose", "--api-key"], "t --verbose --api-key"),
            // `NAME=value` env-style
            (
                vec!["env", "GITHUB_TOKEN=ghp_TEST", "cmd"],
                "env GITHUB_TOKEN=[REDACTED] cmd",
            ),
            (
                vec!["env", "PGPASSWORD=hunter2", "psql"],
                "env PGPASSWORD=[REDACTED] psql",
            ),
            // value shaped like a credential under a plain flag/env name
            (vec!["t", "--header=xoxb-TEST"], "t --header=[REDACTED]"),
            (vec!["t", "URL=sk-TEST"], "t URL=[REDACTED]"),
            // env names carrying secrets by convention
            (
                vec![
                    "env",
                    "DATABASE_URL=postgres://u:p@h/db",
                    "SENTRY_DSN=https://x@y",
                    "SLACK_WEBHOOK=https://z",
                    "DB_CONN=mysql://h",
                ],
                "env DATABASE_URL=[REDACTED] SENTRY_DSN=[REDACTED] SLACK_WEBHOOK=[REDACTED] DB_CONN=[REDACTED]",
            ),
            // ordinary arguments pass through untouched
            (
                vec!["cargo", "test", "--", "--port", "3010", "/tmp/x"],
                "cargo test -- --port 3010 /tmp/x",
            ),
            (
                vec!["t", "--config=/etc/app.conf", "verbose"],
                "t --config=/etc/app.conf verbose",
            ),
            (
                vec!["env", "EDITOR=vim", "URL=https://x/?q=1"],
                "env EDITOR=vim URL=[REDACTED]",
            ),
        ];
        for (argv, want) in cases {
            assert_eq!(redact_argv(&argv), want, "{argv:?}");
        }
    }

    /// The three leak shapes from the CAD-108 round-2 review, plus
    /// the `--flag value` and `--flag=` regressions.
    #[test]
    fn redact_argv_header_uri_and_short_flags() {
        let cases: Vec<(Vec<&str>, &str)> = vec![
            // header-style single arguments — one argv element holds
            // `Name: value` with the space inside
            (
                vec![
                    "curl",
                    "-H",
                    "Authorization: Basic YWxpY2U6c3VwZXJzZWNyZXQ=",
                    "https://api.x",
                ],
                "curl -H Authorization: [REDACTED] https://api.x",
            ),
            (
                vec!["tool", "-H", "X-Api-Key: figd_LIVE"],
                "tool -H X-Api-Key: [REDACTED]",
            ),
            // an innocent header name still loses a credential value
            (
                vec!["tool", "-H", "X-Custom: sk-LIVE"],
                "tool -H X-Custom: [REDACTED]",
            ),
            // URIs with embedded credentials
            (
                vec!["psql", "postgres://admin:hunter2@db.example.com:5432/app"],
                "psql postgres://admin:[REDACTED]@db.example.com:5432/app",
            ),
            (
                vec![
                    "git",
                    "clone",
                    "https://user:ghp_ABCDEFGHIJKLMNOPQRST@github.com/o/r.git",
                ],
                "git clone https://user:[REDACTED]@github.com/o/r.git",
            ),
            // a credential-shaped userinfo without a password
            (
                vec!["git", "clone", "https://ghp_LIVETOKEN@github.com/o/r.git"],
                "git clone https://[REDACTED]@github.com/o/r.git",
            ),
            // a URI secret inside a `NAME=value` / `--flag=` value
            (
                vec!["t", "CONFIG=postgres://u:hunter2@db/x"],
                "t CONFIG=postgres://u:[REDACTED]@db/x",
            ),
            // short attached flags
            (
                vec!["mysql", "-uroot", "-phunter2", "db"],
                "mysql -uroot -p[REDACTED] db",
            ),
            (
                vec!["redis-cli", "-a", "hunter2"],
                "redis-cli -a [REDACTED]",
            ),
            (
                vec!["curl", "-u", "admin:hunter2", "https://x"],
                "curl -u [REDACTED] https://x",
            ),
            (vec!["curl", "-uadmin:hunter2"], "curl -u[REDACTED]"),
            // a `-` value is still the value of a secret flag
            (vec!["t", "--password", "-p123"], "t --password [REDACTED]"),
            // regression: the incident shape stays redacted
            (
                vec!["t", "--figma-api-key=figd_LIVE"],
                "t --figma-api-key=[REDACTED]",
            ),
            (
                vec!["t", "--figma-api-key", "figd_LIVE"],
                "t --figma-api-key [REDACTED]",
            ),
        ];
        for (argv, want) in cases {
            assert_eq!(redact_argv(&argv), want, "{argv:?}");
        }
    }

    /// The over-redaction side of the review — ordinary argv that
    /// must survive untouched.
    #[test]
    fn redact_argv_ordinary_args_survive() {
        let cases: Vec<(Vec<&str>, &str)> = vec![
            (
                vec!["npm", "publish", "--access", "public"],
                "npm publish --access public",
            ),
            (
                vec![
                    "git",
                    "checkout",
                    "4f2a9c1d8e3b5a7c9f1e2d3b4a5c6d7e8f9a0b1c",
                ],
                "git checkout 4f2a9c1d8e3b5a7c9f1e2d3b4a5c6d7e8f9a0b1c",
            ),
            (
                vec!["git", "log", "--oneline", "-n", "deadbeef", "abc1234"],
                "git log --oneline -n deadbeef abc1234",
            ),
            // canonical UUIDs, bare and as a flag value
            (
                vec!["t", "550e8400-e29b-41d4-a716-446655440000"],
                "t 550e8400-e29b-41d4-a716-446655440000",
            ),
            (
                vec!["t", "--uuid=550e8400-e29b-41d4-a716-446655440000"],
                "t --uuid=550e8400-e29b-41d4-a716-446655440000",
            ),
            // keyword inside a word is not a keyword
            (
                vec!["t", "monkey=banana", "bypass=1"],
                "t monkey=banana bypass=1",
            ),
            // `-p` carrying ports for ssh/docker is not a password
            (vec!["ssh", "-p", "2222", "host"], "ssh -p 2222 host"),
            (
                vec!["docker", "run", "-p", "8080:80", "img"],
                "docker run -p 8080:80 img",
            ),
            // bundled short flags are not `-a <secret>`
            (vec!["ps", "-aux"], "ps -aux"),
            (vec!["ps", "-a", "-f"], "ps -a -f"),
            // `-u` with a plain username
            (vec!["mysql", "-u", "root", "db"], "mysql -u root db"),
            // long paths as flag values or standalone args
            (
                vec![
                    "t",
                    "--path=/usr/lib/x86_64-linux-gnu/libsomewhatlongername.so",
                ],
                "t --path=/usr/lib/x86_64-linux-gnu/libsomewhatlongername.so",
            ),
            (
                vec!["t", "/usr/lib/x86_64-linux-gnu/libsomethingverylongname.so"],
                "t /usr/lib/x86_64-linux-gnu/libsomethingverylongname.so",
            ),
        ];
        for (argv, want) in cases {
            assert_eq!(redact_argv(&argv), want, "{argv:?}");
        }
    }

    #[test]
    fn redact_argv_credential_shapes() {
        for token in [
            "figd_TESTTOKEN",
            "ghp_TESTTOKEN",
            "gho_TESTTOKEN",
            "github_pat_TESTTOKEN",
            "sk-TESTTOKEN",
            "xoxb-TESTTOKEN",
            "xoxp-TESTTOKEN",
            // AKIA + 16 uppercase/digits is the whole shape.
            "AKIAXXXXXXXXXXXXXXXX",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5t_Q",
            // 32+ char high-entropy token
            "9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c",
        ] {
            assert_eq!(
                redact_argv(&["tool", token]),
                format!("tool {REDACTED}"),
                "{token}"
            );
        }
        // …but ordinary long args survive: a path (`/` excluded), an
        // all-alpha run (no digit), a short token-looking arg.
        for arg in [
            "/usr/lib/x86_64-linux-gnu/libsomethingverylongname.so",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "short-token",
        ] {
            assert_eq!(redact_argv(&["tool", arg]), format!("tool {arg}"), "{arg}");
        }
        // Under a flag or env name the base64 set applies — an AWS
        // secret access key carries `/` and `+` and may be digit-free.
        let aws_secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        assert_eq!(
            redact_argv(&["t", &format!("--data={aws_secret}")]),
            "t --data=[REDACTED]"
        );
        assert_eq!(
            redact_argv(&["env", &format!("DATA={aws_secret}")]),
            "env DATA=[REDACTED]"
        );
    }

    #[test]
    fn orphans_redact_secret_argv() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = scan.cwd.clone();
        add_pid(
            &scan.proc_root,
            40,
            Some(&repo.join(".cadence/wt/gone")),
            None,
            Some("npm exec figma-developer-mcp --figma-api-key=figd_TESTKEY0002 --stdio"),
            7_200,
            &[],
        );
        let c = check_orphans(&scan);
        // detail, remedy and the serialised JSON all carry `head` —
        // none may contain the credential.
        let blob = serde_json::to_string(&c.to_json()).unwrap();
        for text in [&blob, &c.detail, &c.remedy] {
            assert!(!text.contains("figd_TESTKEY0002"), "{text}");
        }
        assert!(blob.contains("--figma-api-key=[REDACTED]"));
        assert!(blob.contains("figma-developer-mcp"));
    }

    #[test]
    fn orphans_redact_header_and_uri_argv() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = scan.cwd.clone();
        // argv element with a space inside — only the vector form of
        // add_pid can write it.
        add_pid_argv(
            &scan.proc_root,
            41,
            Some(&repo.join(".cadence/wt/gone")),
            None,
            Some(&[
                "curl",
                "-H",
                "Authorization: Basic YWxpY2U6c3VwZXJzZWNyZXQ=",
                "https://api.x",
            ]),
            7_200,
            &[],
        );
        add_pid_argv(
            &scan.proc_root,
            42,
            Some(&repo.join(".cadence/wt/gone")),
            None,
            Some(&["psql", "postgres://admin:hunter2@db:5432/app"]),
            7_200,
            &[],
        );
        let c = check_orphans(&scan);
        let blob = serde_json::to_string(&c.to_json()).unwrap();
        for text in [&blob, &c.detail, &c.remedy] {
            assert!(!text.contains("YWxpY2U6c3VwZXJzZWNyZXQ"), "{text}");
            assert!(!text.contains("hunter2"), "{text}");
        }
        assert!(blob.contains("Authorization: [REDACTED]"), "{blob}");
        assert!(blob.contains("postgres://admin:[REDACTED]@db"), "{blob}");
        // …while the ordinary parts stay readable.
        assert!(blob.contains("curl"), "{blob}");
        assert!(blob.contains("https://api.x"), "{blob}");
    }

    #[test]
    fn deleted_worktree_path_matching() {
        let root = TempDir::new().unwrap();
        let gone = root.path().join("repo/.cadence/wt/x");
        assert!(deleted_worktree(&gone).is_some());
        let live = root.path().join("repo/.cadence/wt/y");
        std::fs::create_dir_all(&live).unwrap();
        assert!(deleted_worktree(&live).is_none());
        assert!(deleted_worktree(Path::new("/usr/bin/bash")).is_none());
        assert!(deleted_worktree(Path::new("/repo/.cadence/wtbak/x")).is_none());
        // The proc "(deleted)" suffix is stripped before the exists check.
        let deleted_marked = PathBuf::from(format!("{} (deleted)", gone.display()));
        assert!(deleted_worktree(&deleted_marked).is_some());
    }

    // ---------- temp dirs ----------

    #[test]
    fn temp_dirs_count_by_prefix_and_age() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        let tmp = scan.temp_dir.clone();
        for name in [
            "cadence-issue-at-1-2",
            "cadence-smoke",
            ".tmpAbc",
            "tmp.XYZ",
        ] {
            std::fs::create_dir_all(tmp.join(name)).unwrap();
        }
        std::fs::create_dir_all(tmp.join("unrelated")).unwrap();
        std::fs::write(tmp.join("cadence-file"), b"not a dir").unwrap();
        // Fresh dirs are below the age threshold — nothing counts.
        let c = check_temp_dirs(&scan);
        assert_eq!(c.level, Level::Ok);
        assert_eq!(c.value["count"].as_u64().unwrap(), 0);
        // Age them past a day; lower the count bar so four dirs warn.
        for name in [
            "cadence-issue-at-1-2",
            "cadence-smoke",
            ".tmpAbc",
            "tmp.XYZ",
        ] {
            set_mtime_old(&tmp.join(name), 90_000);
        }
        scan.thresholds.temp_warn_count = 3;
        let c = check_temp_dirs(&scan);
        assert_eq!(c.level, Level::Warn);
        assert_eq!(c.value["count"].as_u64().unwrap(), 4);
        assert!(c.remedy.contains("rm -rf"));
        assert!(c.remedy.contains("cadence-issue-at-1-2"));
    }

    // ---------- stale worktrees ----------

    #[test]
    fn worktrees_flag_merged_or_closed_only() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = scan.cwd.clone();
        init_repo(&repo);
        // wt1: branch merged into main, clean tree → stale.
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                ".cadence/wt/feat-a",
                "-b",
                "feat-a",
            ],
        );
        std::fs::write(repo.join(".cadence/wt/feat-a/f"), b"x").unwrap();
        git(&repo.join(".cadence/wt/feat-a"), &["add", "-A"]);
        git(
            &repo.join(".cadence/wt/feat-a"),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "work",
            ],
        );
        git(&repo, &["merge", "-q", "feat-a"]);
        // wt2: unmerged branch, no tracker → not stale.
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                ".cadence/wt/feat-b",
                "-b",
                "feat-b",
            ],
        );
        std::fs::write(repo.join(".cadence/wt/feat-b/f2"), b"x").unwrap();
        git(&repo.join(".cadence/wt/feat-b"), &["add", "-A"]);
        git(
            &repo.join(".cadence/wt/feat-b"),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "wip",
            ],
        );
        // wt3: unmerged but tracker says done → stale.
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                ".cadence/wt/cad-1-x",
                "-b",
                "cadence/cad-1-x",
            ],
        );
        std::fs::write(repo.join(".cadence/wt/cad-1-x/f3"), b"x").unwrap();
        git(&repo.join(".cadence/wt/cad-1-x"), &["add", "-A"]);
        git(
            &repo.join(".cadence/wt/cad-1-x"),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "wip",
            ],
        );
        write_issue(
            scan.pm_dir.as_ref().unwrap(),
            "cadence",
            "CAD-1",
            "done",
            "",
        );
        // wt4: merged branch but dirty tree and open tracker → not stale.
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                ".cadence/wt/cad-2-y",
                "-b",
                "cadence/cad-2-y",
            ],
        );
        std::fs::write(repo.join(".cadence/wt/cad-2-y/f4"), b"x").unwrap();
        git(&repo.join(".cadence/wt/cad-2-y"), &["add", "-A"]);
        git(
            &repo.join(".cadence/wt/cad-2-y"),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "work",
            ],
        );
        git(&repo, &["merge", "-q", "cadence/cad-2-y"]);
        write_issue(
            scan.pm_dir.as_ref().unwrap(),
            "cadence",
            "CAD-2",
            "doing",
            "",
        );
        std::fs::write(repo.join(".cadence/wt/cad-2-y/uncommitted"), b"wip").unwrap();

        let c = check_worktrees(&scan);
        assert_eq!(c.level, Level::Warn);
        let mut names: Vec<String> = c.value["stale"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                s["path"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(names, vec!["cad-1-x", "feat-a"]);
        assert!(c.remedy.contains("cadence issue finish CAD-1"));

        // Now close CAD-2's worktree ref in the tracker — dirty tree or
        // not, a closed ref means finished.
        write_issue(
            scan.pm_dir.as_ref().unwrap(),
            "cadence",
            "CAD-2",
            "doing",
            &format!(
                "refs:\n- kind: worktree\n  path: {}\n  closed: true\n",
                repo.join(".cadence/wt/cad-2-y").display()
            ),
        );
        let c = check_worktrees(&scan);
        let mut names: Vec<String> = c.value["stale"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                s["path"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(names, vec!["cad-1-x", "cad-2-y", "feat-a"]);
    }

    #[test]
    fn worktrees_count_shared_target_once() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = scan.cwd.clone();
        init_repo(&repo);
        // Two live lanes, one shared cache — the cache's bytes land
        // once in `shared_cargo_target`, never inside a lane's row.
        for name in ["cad-1-a", "cad-2-b"] {
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    &format!(".cadence/wt/{name}"),
                    "-b",
                    &format!("cadence/{name}"),
                ],
            );
        }
        let shared = repo.join(".cadence/target/shared");
        real_bytes(&shared.join("dep.rlib"), 4 * 1024 * 1024);
        let c = check_worktrees(&scan);
        let st = &c.value["shared_cargo_target"];
        assert_eq!(
            st["path"].as_str().unwrap(),
            shared.to_string_lossy(),
            "{st}"
        );
        assert_eq!(st["bytes"].as_u64().unwrap(), 4 * 1024 * 1024);
        // Exactly one shared entry — a `stale` row per lane never
        // carries the shared bytes with it.
        assert!(c.value["stale"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["path"].as_str().unwrap() != shared.to_string_lossy()));
        assert!(c.detail.contains("shared cargo cache"), "{}", c.detail);
    }

    #[test]
    fn reclaim_plan_lists_without_deleting() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = scan.cwd.clone();
        init_repo(&repo);
        // A live lane with a per-lane target/, the shared cache, and
        // a stale lane with its own target/ — all listed, none
        // deleted, and the stale lane's bytes never count its
        // target/ twice.
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                ".cadence/wt/cad-1-live",
                "-b",
                "cadence/cad-1-live",
            ],
        );
        let live = repo.join(".cadence/wt/cad-1-live");
        // A commit past base keeps the lane genuinely live — a branch
        // at base with a clean tree reads as stale.
        std::fs::write(live.join("wip.txt"), "x").unwrap();
        git(&live, &["add", "-A"]);
        git(
            &live,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "wip",
            ],
        );
        let lane_target = live.join("target");
        real_bytes(&lane_target.join("dep.rlib"), 1024 * 1024);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                ".cadence/wt/feat-gone",
                "-b",
                "feat-gone",
            ],
        );
        let gone = repo.join(".cadence/wt/feat-gone");
        // Committed content keeps the tree clean past the merge; the
        // ignored target/ adds reclaimable bytes without dirtying it.
        real_bytes(&gone.join("notes.txt"), 64 * 1024);
        git(&gone, &["add", "-A"]);
        git(
            &gone,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "notes",
            ],
        );
        git(&repo, &["merge", "-q", "feat-gone"]);
        real_bytes(&gone.join("target/dep.rlib"), 512 * 1024);
        let shared = repo.join(".cadence/target/shared");
        // Reclaimable bytes live in the shared subdirs — and a
        // retired r2-era `examples/` gets its own row.
        real_bytes(&shared.join("debug/deps/dep.rlib"), 2 * 1024 * 1024);
        real_bytes(&shared.join("debug/examples/ex.bin"), 128 * 1024);

        let plan = reclaim_plan(&scan);
        let rows = plan["rows"].as_array().unwrap();
        let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
        assert!(
            kinds.contains(&"worktree-target")
                && kinds.contains(&"shared-cargo-cache")
                && kinds.contains(&"stale-worktree")
                && kinds.contains(&"retired-shared-dir"),
            "{kinds:?}"
        );
        // Live-lane target rows are informational — excluded from the
        // reclaimable total and surfaced on their own line instead.
        assert_eq!(
            plan["reclaimable_bytes"].as_u64().unwrap(),
            rows.iter()
                .filter(|r| r["kind"] != "worktree-target")
                .map(|r| r["bytes"].as_u64().unwrap())
                .sum::<u64>()
        );
        assert_eq!(
            plan["freed_with_lanes_bytes"].as_u64().unwrap(),
            rows.iter()
                .filter(|r| r["kind"] == "worktree-target")
                .map(|r| r["bytes"].as_u64().unwrap())
                .sum::<u64>()
        );
        // A stale lane's whole dir — target/ included — is freed by
        // its own row's command, so its bytes are the full dir.
        for stale in rows.iter().filter(|r| r["kind"] == "stale-worktree") {
            let (whole, _) = dir_size(Path::new(stale["path"].as_str().unwrap()));
            assert_eq!(stale["bytes"].as_u64().unwrap(), whole, "{stale}");
        }
        let gone_row = rows
            .iter()
            .find(|r| {
                r["kind"] == "stale-worktree" && r["path"].as_str().unwrap().ends_with("feat-gone")
            })
            .unwrap();
        assert!(gone_row["bytes"].as_u64().unwrap() >= 512 * 1024);
        // A stale lane never also emits an informational target row.
        assert!(!rows.iter().any(|r| r["kind"] == "worktree-target"
            && r["path"]
                .as_str()
                .unwrap()
                .starts_with(gone.to_str().unwrap())));
        // A live lane's target row describes how it frees — it never
        // reads as "finish your in-progress work".
        let live = rows
            .iter()
            .find(|r| r["kind"] == "worktree-target")
            .unwrap();
        assert!(live["action"]
            .as_str()
            .unwrap()
            .contains("freed with the lane"));
        // Every row names its action and filesystem; nothing deleted.
        assert!(rows
            .iter()
            .all(|r| !r["action"].as_str().unwrap().is_empty()));
        assert!(lane_target.is_dir() && shared.is_dir());
        assert!(gone.is_dir());
        let text = render_reclaim(&plan);
        assert!(
            text.contains("total reclaimable") && text.contains("shared-cargo-cache"),
            "{text}"
        );
    }

    #[test]
    fn reclaim_plan_quotes_paths_and_reports_lock() {
        // A repo whose path contains a space — unquoted `rm -rf`
        // would split it into extra arguments.
        let root = tempfile::Builder::new()
            .prefix("my proj ")
            .tempdir()
            .unwrap();
        let mut scan = fake_scan(&root);
        let repo = root.path().join("repo dir");
        std::fs::create_dir_all(&repo).unwrap();
        scan.cwd = repo.clone();
        init_repo(&repo);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                ".cadence/wt/feat gone",
                "-b",
                "feat-gone",
            ],
        );
        git(&repo, &["merge", "-q", "feat-gone"]);
        let shared = repo.join(".cadence/target/shared");
        real_bytes(&shared.join("dep.rlib"), 4096);

        let plan = reclaim_plan(&scan);
        let rows = plan["rows"].as_array().unwrap();
        // Every emitted command's path args round-trip through a real
        // shell word-split: `set -- <quoted>` must hand back exactly
        // the original paths.
        let actions: Vec<String> = rows
            .iter()
            .map(|r| r["action"].as_str().unwrap().to_string())
            .collect();
        let stale = actions
            .iter()
            .find(|a| a.contains("worktree remove"))
            .expect("stale row")
            .clone();
        // The git -C line: `git -C <root> worktree remove <path>` —
        // extract the two path args and ask `sh` to split them.
        let (root_q, path_q) = stale
            .strip_prefix("git -C ")
            .unwrap()
            .split_once(" worktree remove ")
            .unwrap();
        for (q, want) in [
            (root_q, repo.display().to_string()),
            (
                path_q,
                repo.join(".cadence/wt/feat gone").display().to_string(),
            ),
        ] {
            let out = Command::new("sh")
                .arg("-c")
                .arg(format!("set -- {q}; printf %s \"$1\""))
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{stale}");
        }
        // The shared row's rm -rf clears *contents*, one quoted glob
        // per shared subdir — run it through a real `sh` and prove
        // the dirs themselves (every lane's symlink target) survive.
        let rm = actions
            .iter()
            .find(|a| a.contains("rm -rf"))
            .expect("shared row")
            .clone();
        let d = shared.join("debug");
        for name in ["deps", ".fingerprint", "build", "incremental"] {
            std::fs::create_dir_all(d.join(name)).unwrap();
            std::fs::write(d.join(name).join("cached.o"), b"x").unwrap();
        }
        let rm_cmd = rm.split("  #").next().unwrap();
        let out = Command::new("sh").arg("-c").arg(rm_cmd).output().unwrap();
        assert!(out.status.success(), "{rm}");
        for name in ["deps", ".fingerprint", "build", "incremental"] {
            let dir = d.join(name);
            assert!(dir.is_dir(), "{name} must survive for lane symlinks");
            assert!(
                std::fs::read_dir(&dir).unwrap().next().is_none(),
                "{name} emptied"
            );
        }
        // And the quoted glob args word-split correctly: the first
        // arg expands inside the space-containing path.
        std::fs::write(d.join("deps/marker"), b"x").unwrap();
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "set -- {}; printf %s \"$1\"",
                rm_cmd.strip_prefix("rm -rf ").unwrap()
            ))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            d.join("deps/marker").display().to_string()
        );

        // The lock assertion is independent of stale-worktree quoting.
        // Retire that linked worktree before the held-lock scan so this
        // phase exercises only shared-cache lock handling and does not
        // launch unrelated git probes.
        git(
            &repo,
            &["worktree", "remove", "-f", ".cadence/wt/feat gone"],
        );

        // A held .cargo-lock swaps the rm -rf for an idle note — and
        // with no freeing command emitted, the row's bytes leave the
        // reclaimable total too.
        let lock = shared.join("debug/.cargo-lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "").unwrap();
        std::fs::write(d.join("deps/cached2.o"), vec![7u8; 8192]).unwrap();
        let f = crate::worktree::TestFileLock::acquire(&lock);
        let plan = reclaim_plan(&scan);
        let shared_row = plan["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "shared-cargo-cache")
            .unwrap()
            .clone();
        assert!(shared_row["cargo_locked"].as_bool().unwrap());
        assert!(shared_row["action"]
            .as_str()
            .unwrap()
            .contains("cargo build"));
        assert!(shared_row["bytes"].as_u64().unwrap() >= 8192);
        let stale_bytes: u64 = plan["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "stale-worktree")
            .map(|r| r["bytes"].as_u64().unwrap())
            .sum();
        assert_eq!(
            plan["reclaimable_bytes"].as_u64().unwrap(),
            stale_bytes,
            "locked shared row must not count toward the total"
        );
        f.release(); // probe must see the lock released
        assert!(!file_locked(&lock));
    }

    #[test]
    fn worktrees_skip_when_no_repo() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        scan.cwd = root.path().join("nowhere");
        std::fs::create_dir_all(&scan.cwd).unwrap();
        let c = check_worktrees(&scan);
        assert_eq!(c.level, Level::Ok);
        assert!(c.value["skipped"].as_bool().unwrap());
    }

    // ---------- plumbing ----------

    #[test]
    fn exit_code_maps_worst_level() {
        assert_eq!(exit_code(&json!({"level": "ok"})), 0);
        assert_eq!(exit_code(&json!({"level": "warn"})), 1);
        assert_eq!(exit_code(&json!({"level": "fail"})), 2);
        assert_eq!(exit_code(&json!({})), 0);
    }

    #[test]
    fn run_emits_all_checks() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        scan.pm_dir = None; // nothing anywhere — cleanest possible host
        let report = run(&scan);
        let names: Vec<&str> = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "disk",
                "provider-state",
                "pipes",
                "memory",
                "processes",
                "orphans",
                "temp-dirs",
                "worktrees"
            ]
        );
        for c in report["checks"].as_array().unwrap() {
            for k in ["level", "value", "threshold", "detail", "remedy"] {
                assert!(c.get(k).is_some(), "check missing {k}");
            }
        }
        assert_eq!(exit_code(&report), 0, "{}", render(&report));
    }

    #[test]
    fn pm_yaml_host_table_overrides() {
        let root = TempDir::new().unwrap();
        let pm = root.path().join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        assert!(host_overrides(&pm).is_none());
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  wal_fail_bytes: 5\n  temp_warn_count: 2\n  mem_warn_pct: 25\n  wal_max_bytes: 4096\n",
        )
        .unwrap();
        let o = host_overrides(&pm).unwrap();
        assert_eq!(o.wal_fail_bytes, Some(5));
        assert_eq!(o.temp_warn_count, Some(2));
        assert_eq!(o.mem_warn_pct, Some(25.0));
        assert_eq!(o.wal_max_bytes, Some(4096));
        let t = Thresholds::resolve(Some(o));
        assert_eq!(t.wal_fail_bytes, 5);
        assert_eq!(t.temp_warn_count, 2);
        assert_eq!(t.mem_warn_pct, 25.0);
        assert_eq!(t.wal_max_bytes, 4096);
        assert_eq!(t.disk_warn_pct, 15.0); // untouched keys keep defaults
        assert_eq!(t.mem_fail_pct, 5.0);
        // A pm.yaml without the table is fine too.
        std::fs::write(pm.join("pm.yaml"), "schema: 1\n").unwrap();
        assert!(host_overrides(&pm).is_none());
    }
}
