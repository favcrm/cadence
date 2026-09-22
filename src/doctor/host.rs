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
//! command an operator would run, except `task-targets`, whose remedy
//! is a read-only inventory note and never a deletion. Nothing here
//! writes, signals or deletes: filesystem reads, `/proc` walks and a
//! handful of read-only `git` probes are the whole surface. The exit
//! code is the worst level: 0 all ok, 1 any warn, 2 any fail.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
/// How many temp-dir entries the legacy task-target scan will look at
/// before it stops and reports the inventory as truncated.
const TASK_TARGET_TEMP_BUDGET: usize = 8_192;
/// Rows kept after that scan. The host's leaked `cad*-target*` set is
/// small; the cap is what keeps a polluted temp dir from blowing the
/// report up.
const TASK_TARGET_ROW_CAP: usize = 64;
/// Shared `stat` budget for every task-target walk in one report, and
/// the most entries one directory may contribute. A real cargo tree
/// trips the per-dir cap (the byte count is then a lower bound and the
/// row warns); a fixture with a handful of files does not.
const TASK_TARGET_STAT_BUDGET: usize = 8_192;
const TASK_TARGET_DIR_STAT_CAP: usize = 1_024;
/// `/proc` pids inspected while attributing cwd/exe to those rows.
const TASK_TARGET_PROC_BUDGET: usize = 8_192;
/// Issue files read while looking for recorded `cargo_target` paths.
const TASK_TARGET_ISSUE_BUDGET: usize = 4_096;
/// Pids quoted on one row. The count is the full number observed.
const TASK_TARGET_PIDS: usize = 8;

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
#[serde(deny_unknown_fields)]
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
    /// CAD-113 build slots — read by the daemon's slot service, not by
    /// `Thresholds`. `build_slots` bounds concurrent build+test grants
    /// (default 3), `suite_slots` the full-suite pool (default 1),
    /// `jobs_per_lane` is the `CARGO_BUILD_JOBS` dispatch injects
    /// (default 4), `starve_secs` is the never-starve bound (default
    /// 900), `priority_lanes` are aliases whose test/suite requests
    /// outrank ordinary ones (the reviewer lane), `max_hold_secs`
    /// reaps a forgotten hold (default 7200).
    pub build_slots: Option<u64>,
    pub suite_slots: Option<u64>,
    pub jobs_per_lane: Option<u64>,
    pub starve_secs: Option<u64>,
    pub priority_lanes: Option<Vec<String>>,
    pub max_hold_secs: Option<u64>,
    /// Load watchdog: warn when load1 exceeds `load_warn_ratio`×cpus
    /// or io stall avg10 exceeds `io_stall_warn_pct`/`io_stall_fail_pct`%.
    /// Unset `load_warn_ratio` derives the warn line from the slot
    /// plan — the farm is *meant* to run `slots × jobs` deep, so warn
    /// above that plan, not below it.
    pub load_warn_ratio: Option<f64>,
    pub io_stall_warn_pct: Option<f64>,
    pub io_stall_fail_pct: Option<f64>,
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
    /// Warn when load1 exceeds this × cpu count (fail at 2×). `None`
    /// derives the line from the slot plan at check time — the farm
    /// is meant to run `slots × jobs` deep, so warn above the plan,
    /// not below it.
    pub load_warn_ratio: Option<f64>,
    /// Warn/fail on `/proc/pressure/io` `some avg10` percent.
    pub io_stall_warn_pct: f64,
    pub io_stall_fail_pct: f64,
    /// Why `pm.yaml [host]` could not be applied, when it could not.
    /// Every threshold is then the default and `wal_checkpoint` is
    /// off: the writer into other tools' stores fails closed (CAD-189).
    pub config_error: Option<String>,
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
            load_warn_ratio: None,
            io_stall_warn_pct: 30.0,
            io_stall_fail_pct: 60.0,
            config_error: None,
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
            if let Some(v) = o.load_warn_ratio {
                t.load_warn_ratio = Some(v);
            }
            if let Some(v) = o.io_stall_warn_pct {
                t.io_stall_warn_pct = v;
            }
            if let Some(v) = o.io_stall_fail_pct {
                t.io_stall_fail_pct = v;
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
    /// `CARGO_TARGET_DIR` as this process received it — absolute, or
    /// relative to `cwd`. `None` when unset. The check reads this
    /// field and never the environment, so a test does not inherit
    /// the runner's target dir.
    pub cargo_target_dir: Option<PathBuf>,
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
    /// The daemon's `slot_status` payload when it answers — `cli()`
    /// fills it best-effort so the load check can report the queue;
    /// `None` means daemon unreachable (reported, not penalised).
    pub slots: Option<Value>,
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
            cargo_target_dir: std::env::var_os("CARGO_TARGET_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
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
            slots: crate::client::rpc_timeout(
                state_dir,
                "slot_status",
                serde_json::json!({"lane": crate::slots::default_lane()}),
                std::time::Duration::from_secs(2),
            )
            .ok(),
            fs_probe: None,
            census: std::cell::OnceCell::new(),
        }
    }
}

/// The optional `[host]` table in `pm.yaml` — read as plain YAML so a
/// missing or older `pm.yaml` is simply "no overrides", never an error.
/// `pub(crate)` — the daemon's slot service and `issue start` reuse it
/// for the CAD-113 `[host]` keys (`build_slots` &c.).
pub(crate) fn host_overrides(pm_dir: &Path) -> Option<HostOverrides> {
    read_host_overrides(pm_dir).ok().flatten()
}

/// `pm.yaml [host]`: `Ok(None)` when there is no file or no table,
/// `Err` naming the problem when the table is present but unusable —
/// a bad value or an unknown (misspelled) key.
pub(crate) fn read_host_overrides(
    pm_dir: &Path,
) -> std::result::Result<Option<HostOverrides>, String> {
    let Ok(text) = std::fs::read_to_string(pm_dir.join("pm.yaml")) else {
        return Ok(None);
    };
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| format!("pm.yaml is not valid YAML: {e}"))?;
    let Some(host) = yaml.get("host") else {
        return Ok(None);
    };
    serde_yaml::from_value(host.clone()).map(Some).map_err(|e| {
        // A type error does not say which key; retry each key alone so
        // the message names the one to fix.
        let culprit = host.as_mapping().and_then(|m| {
            m.iter().find_map(|(k, v)| {
                let mut one = serde_yaml::Mapping::new();
                one.insert(k.clone(), v.clone());
                serde_yaml::from_value::<HostOverrides>(serde_yaml::Value::Mapping(one))
                    .is_err()
                    .then(|| k.as_str().unwrap_or("?").to_string())
            })
        });
        match culprit {
            Some(key) => format!("pm.yaml [host] {key}: {e}"),
            None => format!("pm.yaml [host]: {e}"),
        }
    })
}

/// Resolved `[host]` thresholds — shared by `Scan::host` and the
/// daemon's WAL watcher so both read one config table.
pub(crate) fn host_thresholds(pm_dir: Option<&Path>) -> Thresholds {
    match pm_dir.map(read_host_overrides).transpose() {
        Ok(overrides) => Thresholds::resolve(overrides.flatten()),
        Err(e) => Thresholds {
            wal_checkpoint: false,
            config_error: Some(e),
            ..Thresholds::default()
        },
    }
}

/// Every host check against `scan`; the report is one JSON object
/// whose `level` is the worst check level.
pub fn run(scan: &Scan) -> Value {
    let checks = [
        check_disk(scan),
        check_provider_state(scan),
        check_pipes(scan),
        check_memory(scan),
        check_processes(scan),
        check_sessions(scan),
        check_orphans(scan),
        check_temp_dirs(scan),
        check_task_targets(scan),
        check_worktrees(scan),
        check_load(scan),
        check_config(scan),
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
/// anything that vanishes mid-walk — a watchdog walk races with the
/// processes it watches. A directory that exists but cannot be listed
/// marks the walk truncated: the byte count is a lower bound, not a
/// complete measurement. Returns `(bytes, truncated)`.
fn dir_size(path: &Path) -> (u64, bool) {
    let (bytes, truncated, _) = dir_size_limited(path, DIR_WALK_BUDGET);
    (bytes, truncated)
}

/// `dir_size` with a caller-chosen entry budget. The third value is
/// how many entries were stat'd. `ent.metadata()` does not follow
/// symlinks; the root `metadata` call does, so callers must not pass
/// a symlink they have refused to follow.
fn dir_size_limited(path: &Path, budget: usize) -> (u64, bool, usize) {
    let mut total = 0u64;
    let mut visited = 0_usize;
    let mut inodes = std::collections::HashSet::new();
    let root_dev = std::fs::metadata(path).ok().map(|m| m.dev());
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // Absence is an empty measurement. Permission and I/O
            // failures are a lower bound: `truncated == false` would
            // otherwise look like a finished walk of nothing.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if dir.as_path() == path {
                    return (0, false, 0);
                }
                continue;
            }
            Err(_) => return (total, true, visited),
        };
        for ent in entries {
            let ent = match ent {
                Ok(ent) => ent,
                Err(_) => return (total, true, visited),
            };
            if visited >= budget {
                return (total, true, visited);
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
    (total, false, visited)
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

/// The fields `stat` yields for free: comm, parentage, CPU, the
/// start-time half of pid+start identity (field 22) and RSS.
struct ProcStat {
    comm: String,
    ppid: u32,
    cpu_jiffies: u64,
    start_jiffies: u64,
    rss_bytes: u64,
}

fn proc_stat(pid_dir: &Path) -> Option<ProcStat> {
    let text = std::fs::read_to_string(pid_dir.join("stat")).ok()?;
    let (head, rest) = text.rsplit_once(')')?;
    let comm = head.split_once('(')?.1.trim().to_string();
    let f: Vec<&str> = rest.split_whitespace().collect();
    // rest[0] is field 3 (state); ppid is field 4 → rest[1].
    let ppid: u32 = f.get(1)?.parse().ok()?;
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
    Some(ProcStat {
        comm,
        ppid,
        cpu_jiffies: utime.saturating_add(stime),
        start_jiffies,
        rss_bytes: rss,
    })
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
        "cursor",
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
        let Some(stat) = proc_stat(&ent.path()) else {
            // A pid that vanished mid-scan is normal; an unreadable
            // stat is hidepid or a race — counted either way.
            if ent.path().exists() {
                census.unreadable += 1;
            } else {
                census.vanished += 1;
            }
            continue;
        };
        let age = uptime.map(|u| (u as u64).saturating_sub(stat.start_jiffies / hz));
        let cpu_secs = stat.cpu_jiffies / hz;
        // "Idle": alive over an hour at under ~1% duty — the leaked
        // browser sessions of CAD-154 burned nothing for days.
        let idle =
            age.is_some_and(|a| a >= 3_600) && cpu_secs.saturating_mul(100) <= age.unwrap_or(0);
        let group = census.groups.entry(comm_family(&stat.comm)).or_default();
        group.count += 1;
        group.rss_bytes += stat.rss_bytes;
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

/// Display-text boundary. ASCII whitespace and Unicode spaces (NBSP,
/// em space, and the rest of `char::is_whitespace`) both count. A
/// shell does not split on NBSP; a flag and value divided by one are
/// still ambiguous credential-bearing diagnostic text, so the value
/// must not stay visible.
fn diagnostic_ws(c: char) -> bool {
    c.is_whitespace()
}

fn has_diagnostic_ws(s: &str) -> bool {
    s.chars().any(diagnostic_ws)
}

/// One argv element is a shell command when it carries whitespace and
/// is not already a single header or a secret-named assignment. Those
/// two stay whole: splitting `Authorization: Basic …` or
/// `--password=two words` would put the tail back on the page.
fn shell_blob_arg(arg: &str) -> bool {
    has_diagnostic_ws(arg) && !header_unit(arg) && !sealed_assignment(arg)
}

/// `Name: value` as one element — the colon rule already redacts the
/// rest, so a nested split must not reopen it. A colon whose left side
/// itself contains whitespace is a command (`psql postgres://…`), not
/// a header.
fn header_unit(arg: &str) -> bool {
    let colon_first = match (arg.find(':'), arg.find('=')) {
        (Some(c), Some(e)) => c < e,
        (Some(_), None) => true,
        _ => false,
    };
    if !colon_first {
        return false;
    }
    let (left, right) = arg.split_once(':').unwrap();
    if has_diagnostic_ws(left) {
        return false;
    }
    secret_name(left)
        || right
            .split_whitespace()
            .any(|seg| secret_value(seg.trim_matches(|c: char| matches!(c, '"' | '\''))))
}

/// `NAME=value` / `--flag=value` whose name owns the whole element.
/// A secret name redacts every character after `=`, spaces included.
/// A plain name with no space in the value is one token; a space means
/// later shell words (`EDITOR=vim cmd --token …`) and must be split.
fn sealed_assignment(arg: &str) -> bool {
    let Some((name, value)) = arg.split_once('=') else {
        return false;
    };
    if name.is_empty() || has_diagnostic_ws(name) {
        return false;
    }
    let flagged = name.starts_with('-');
    let bare = name.trim_start_matches('-');
    if bare.is_empty() || !(flagged || env_name(name)) {
        return false;
    }
    if secret_name(bare) {
        return true;
    }
    !has_diagnostic_ws(value)
}

/// `sh -c` is the outer element (depth 0). Each quoted command inside
/// it opens one more layer, up to this depth. Past it, secret-bearing
/// text is withheld instead of parsed further.
const SHELL_BLOB_DEPTH: u8 = 2;

struct ShellWord<'a> {
    start: usize,
    end: usize,
    raw: &'a str,
    logical: String,
}

/// Bounded word split for one command element. Single and double
/// quotes group a word (so a value can contain spaces); a quote
/// mid-word (`don't`) stays literal. Unicode spaces are character
/// boundaries in the diagnostic text, not shell metacharacters.
/// `$`, backticks and backslashes are expansions or escapes — the
/// split fails closed instead of guessing. Nothing here is executed.
fn split_shell_words(s: &str) -> std::result::Result<Vec<ShellWord<'_>>, ()> {
    let b = s.as_bytes();
    let mut words = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let Some(ch) = s[i..].chars().next() else {
            break;
        };
        if diagnostic_ws(ch) {
            i += ch.len_utf8();
            continue;
        }
        let start = i;
        while i < b.len() {
            let Some(ch) = s[i..].chars().next() else {
                break;
            };
            if diagnostic_ws(ch) {
                break;
            }
            match b[i] {
                b'\\' | b'$' | b'`' => return Err(()),
                b'\'' | b'"' if i == start || b[i - 1] == b'=' => {
                    let q = b[i];
                    i += 1;
                    let mut closed = false;
                    while i < b.len() {
                        if q == b'"' && matches!(b[i], b'\\' | b'$' | b'`') {
                            return Err(());
                        }
                        if b[i] == q {
                            i += 1;
                            closed = true;
                            break;
                        }
                        i += s[i..].chars().next().map_or(1, char::len_utf8);
                    }
                    if !closed {
                        return Err(());
                    }
                }
                _ => i += s[i..].chars().next().map_or(1, char::len_utf8),
            }
        }
        let raw = &s[start..i];
        words.push(ShellWord {
            start,
            end: i,
            logical: logical_shell_word(raw),
            raw,
        });
    }
    Ok(words)
}

fn logical_shell_word(raw: &str) -> String {
    strip_shell_value_quotes(unwrap_shell_quotes(raw))
}

fn unwrap_shell_quotes(raw: &str) -> &str {
    let b = raw.as_bytes();
    if b.len() >= 2 && matches!(b[0], b'\'' | b'"') && b[b.len() - 1] == b[0] {
        return &raw[1..b.len() - 1];
    }
    raw
}

/// `--token="value"` and `NAME='value'` classify as the unquoted
/// forms the flag and env rules already know. The quotes are dropped
/// even when more characters follow them (`TOKEN="x"extra`) so a
/// secret name still owns the value.
fn strip_shell_value_quotes(s: &str) -> String {
    let b = s.as_bytes();
    let Some(eq) = b.iter().position(|c| *c == b'=') else {
        return s.to_string();
    };
    if eq + 1 >= b.len() {
        return s.to_string();
    }
    let q = b[eq + 1];
    if q != b'\'' && q != b'"' {
        return s.to_string();
    }
    let rest = &s[eq + 2..];
    let Some(rel) = rest.find(q as char) else {
        return s.to_string();
    };
    let mut out = String::with_capacity(s.len() - 2);
    out.push_str(&s[..=eq]);
    out.push_str(&rest[..rel]);
    out.push_str(&rest[rel + 1..]);
    out
}

/// Tokenizer gave up. Withhold the element when a flag, assignment,
/// header or credential shape is still visible; benign text (an
/// unmatched quote, `echo $HOME`) stays so a failed split is not a
/// blanket mask.
fn blob_may_hold_secret(s: &str) -> bool {
    if s.contains("://") && s.contains('@') {
        return true;
    }
    for raw in s.split_whitespace() {
        let tok = raw.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | '('));
        if tok.is_empty() {
            continue;
        }
        if looks_like_credential(tok) {
            return true;
        }
        if let Some(body) = tok.strip_prefix('-') {
            let name = body.split(['=', ':']).next().unwrap_or(body);
            if secret_name(name) {
                return true;
            }
            if !tok.starts_with("--") && matches!(name.chars().next(), Some('p' | 'a' | 'u')) {
                return true;
            }
        }
        if let Some((name, value)) = tok.split_once('=') {
            let name = name.trim_matches(|c: char| matches!(c, '"' | '\''));
            let value = value.trim_matches(|c: char| matches!(c, '"' | '\''));
            if secret_name(name.trim_start_matches('-'))
                || secret_value(value)
                || looks_like_credential(value)
            {
                return true;
            }
        }
        if let Some((name, value)) = tok.split_once(':') {
            let name = name.trim_matches(|c: char| matches!(c, '"' | '\''));
            if secret_name(name) {
                return true;
            }
            let value = value.trim_matches(|c: char| matches!(c, '"' | '\''));
            if secret_value(value) || looks_like_credential(value) {
                return true;
            }
        }
    }
    false
}

fn withhold_or_keep(s: &str) -> String {
    if blob_may_hold_secret(s) {
        REDACTED.to_string()
    } else {
        s.to_string()
    }
}

/// Apply the flat argv rules inside one command element and splice
/// the results back, keeping the original whitespace and quoting
/// wherever a word did not change.
fn redact_shell_blob(s: &str, depth: u8) -> String {
    if depth > SHELL_BLOB_DEPTH {
        return withhold_or_keep(s);
    }
    let words = match split_shell_words(s) {
        Ok(words) => words,
        Err(()) => return withhold_or_keep(s),
    };
    if words.is_empty() {
        return s.to_string();
    }
    let logicals: Vec<String> = words
        .iter()
        .map(|w| {
            if has_diagnostic_ws(&w.logical)
                && !header_unit(&w.logical)
                && !sealed_assignment(&w.logical)
            {
                redact_shell_blob(&w.logical, depth + 1)
            } else {
                w.logical.clone()
            }
        })
        .collect();
    let redacted = redact_argv_parts(&logicals, false, true);
    let keep = redacted.len().min(words.len());
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for i in 0..keep {
        out.push_str(&s[pos..words[i].start]);
        if redacted[i] == words[i].logical {
            out.push_str(words[i].raw);
        } else {
            out.push_str(&redacted[i]);
        }
        pos = words[i].end;
    }
    if keep == words.len() {
        out.push_str(&s[pos..]);
    }
    out
}

/// Redact credential material from an argv, returning it joined with
/// spaces — the one place argv becomes display text. The executable
/// and ordinary arguments pass through; secret *values* become
/// `[REDACTED]`: the value of any `--flag=value` or `--flag value`
/// whose flag name looks credential-bearing, `NAME=value` env-style
/// arguments with such a name, `Key: value` header arguments, the
/// password half of `scheme://user:pass@host`, `-p`/`-a`/`-u`
/// short-flag values, and any standalone argument matching a known
/// credential shape or the 32+ char high-entropy token shape.
///
/// An element that itself holds a command (`sh -c 'run --token …'`)
/// is split once on unquoted whitespace, including Unicode spaces,
/// and run through those same rules, so a secret inside the script
/// is not printed. A Unicode space is not shell syntax; it is only
/// a character boundary so a flag value cannot hide against the
/// flag. Quotes are grouping only — this is not a shell parser and
/// it never executes the text. `$`, backticks and backslashes fail
/// closed: the element
/// is withheld when a secret flag, assignment, header or credential
/// shape is still visible, and left unchanged when it is not. A
/// secret-named `NAME=value` that occupies the whole element still
/// redacts the entire value, trailing words included.
/// Public because `src/session.rs` shares it — argv-as-text must
/// share one scrubber rather than re-implement.
pub fn redact_argv<S: AsRef<str>>(argv: &[S]) -> String {
    redact_argv_parts(argv, true, false).join(" ")
}

/// `blobs`: a top-level element that is a command string is split.
/// `header_tail`: inside that split, a bare `Name:` consumes the
/// following words so the header value cannot survive beside it.
fn redact_argv_parts<S: AsRef<str>>(argv: &[S], blobs: bool, header_tail: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_ref();
        if blobs && shell_blob_arg(arg) {
            out.push(redact_shell_blob(arg, 0));
            i += 1;
            continue;
        }
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
                    // A bare `Name:` inside a command string is only
                    // the header; the value is the words after it.
                    // Eating them here is what keeps
                    // `Authorization: Basic <secret>` from printing
                    // once the colon no longer shares their element.
                    if header_tail && right.trim().is_empty() {
                        return out;
                    }
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
    out
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

// ---------- owned session trees (CAD-198; CAD-188 phase 1) ----------
//
// Read-only census answering "which agent session trees are on this
// host, who owns each, and what would the unowned in-scope ones free".
// Ownership is a three-way join — registry row (UNSCOPED), the recorded
// endpoint pid, the live process tree — and every disagreement mode is
// named rather than guessed. Nothing here acts: no kills, stops,
// deletes, checkpoints or writes of any kind.
//
// Rules carried from the ops audit
// (/var/www/agent-notes/20260920-160600-ops-cad188-session-gc-audit.md):
//   * The registry read is unscoped BY CONSTRUCTION — the store opens
//     read-only and every agents row is read. `cadence agent list`
//     silently group-scopes under CADENCE_ALIAS; a scoped absence must
//     never prove "unowned" (12 of 20 agents were invisible that way).
//     `registry_scope` is printed so the consumer can see which view
//     produced the classification.
//   * Tree totals are PSS (smaps_rollup) and VmSwap (status) summed
//     once per member pid — never RSS sums and never sums of
//     per-family totals: MCP wrappers are children of the sessions
//     that spawned them, so their cost is already inside the tree.
//   * argv and env are never opened (CAD-141); identity is comm, the
//     (pid, start_jiffies) pair and the cwd/exe links only — and
//     start_jiffies is enforced, not just printed: a recorded
//     endpoint pid that resolves to a process newer than the row's
//     last write is reuse, and no claim may ride on it.
//   * A tree is a reclaim candidate only when ownership is proven
//     absent (ProcessOnly on a fully-read registry) AND the root runs
//     as the daemon's euid AND its cwd lands inside this pm's
//     registered projects — in the same mount namespace, on a live
//     (not deleted) cwd, under scope roots that are themselves
//     provable directories. Foreign users, foreign projects,
//     disputed ownership and unreadable evidence are all listed but
//     protected.

/// comm families that can root an agent session tree — the providers
/// cadence spawns (managed stdio) or a pane can run (pty). Membership
/// is by kernel comm alone; argv is never opened. A provider added
/// without a row here is silently missed by the census — fail-open on
/// *listing* only (the session is invisible, never misclaimed as a
/// candidate or an owner).
const SESSION_FAMILIES: &[&str] = &["claude", "codex", "cursor", "devin"];

/// `VmSwap`/`Pss` reads are per-pid; an `smaps_rollup` absent or denied
/// for part of a tree marks the totals partial, never wrong.
struct TreeMetrics {
    pss_bytes: u64,
    swap_bytes: u64,
    pss_missing: u32,
    swap_missing: u32,
    /// The member list outgrew `MAX_METRIC_PIDS` — totals are a lower
    /// bound over the first N members only.
    truncated: bool,
}

/// Per-member `smaps_rollup`/`status` reads are capped — a session
/// that forked a build must not make `doctor` map-walk the whole
/// build on every `session start`/`session end`. Over the cap the
/// tree's totals are a flagged lower bound, never an estimate.
const MAX_METRIC_PIDS: usize = 512;

/// How a live session tree and the registry agree — the CAD-188
/// closed set minus `RecordOnly` (a registry row with no live tree is
/// not a tree; those rows are listed under `records_only`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Agreement {
    /// Registry row + live tree, joined on the recorded endpoint pid.
    Agreed,
    /// Live tree, no registry row — proven against an unscoped read
    /// of a store that opened (or is provably absent: no rows exist).
    ProcessOnly,
    /// The registry generation contradicts the generation a live
    /// running turn token embeds — fence, never a candidate.
    GenerationMismatch,
    /// The join could not be proven — store unreadable, endpoint pid
    /// claimed twice, /proc raced. Fail-closed: never a candidate.
    Unknown,
}

impl Agreement {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agreed => "agreed",
            Self::ProcessOnly => "process-only",
            Self::GenerationMismatch => "generation-mismatch",
            Self::Unknown => "unknown",
        }
    }
}

/// Where the tree's root cwd sits relative to this pm's projects —
/// scope is proven by path, never by alias-name pattern.
#[derive(Clone, PartialEq, Eq)]
enum Scope {
    /// Inside a registered project repo (its key) or the pm dir.
    Project(String),
    /// Resolved but matching nothing registered — a foreign project
    /// or unrelated session; protected, never a candidate.
    Foreign,
    /// cwd unreadable, deleted, or in a different mount namespace —
    /// scope cannot be proven.
    Unproven,
}

impl Scope {
    fn as_str(&self) -> &str {
        match self {
            Self::Project(k) => k,
            Self::Foreign => "foreign",
            Self::Unproven => "unproven",
        }
    }
}

/// One live session tree — the census row.
struct SessionTree {
    root_pid: u32,
    /// starttime jiffies — pid+start is the identity; pid alone is
    /// not (PID reuse is CAD-188 §10).
    root_start_jiffies: u64,
    /// The root's real uid (`status` Uid:) — a session owned by
    /// another user is never a candidate, whatever its cwd says.
    /// `None` = unreadable → uid unproven → still not a candidate.
    root_uid: Option<u32>,
    /// The root's `ns/mnt` differs from ours — its cwd resolves in
    /// another namespace, so path-based scope cannot be proven.
    root_ns_foreign: bool,
    root_family: String,
    root_cwd: Option<PathBuf>,
    root_cwd_deleted: bool,
    root_cpu_secs: u64,
    age_secs: Option<u64>,
    /// Every member's (pid, start_jiffies) — attribution and a later
    /// phase's recheck material; each pid appears in one tree only.
    members: Vec<(u32, u64)>,
    metrics: TreeMetrics,
    owner: Option<usize>,
    agreement: Agreement,
    agreement_why: String,
    scope: Scope,
}

/// One `agents` row plus the message facts the census can prove.
struct RegAgent {
    alias: String,
    provider: String,
    endpoint_kind: String,
    generation: Option<String>,
    /// agents.pid — the pane pid for pty endpoints, the provider
    /// process for managed ones; a stale value is a fact, not an
    /// error.
    endpoint_pid: Option<u32>,
    /// agents.cwd — where the row's endpoint lives. Used to fence
    /// "unowned" when the recorded pid has gone stale: a tree rooted
    /// under a stale row's cwd is plausibly that row's session.
    cwd: Option<String>,
    state: String,
    reason: Option<String>,
    queued: u64,
    running: u64,
    /// Generation embedded in a live running turn token
    /// (`<kind>-<generation>-<uuid>`) — the only live-endpoint
    /// generation readable without the daemon.
    running_generation: Option<String>,
    /// Newest completed/started message time, else the row's updated.
    last_progress: Option<f64>,
    /// agents.updated — the row's last write. An endpoint pid is
    /// recorded together with a write, so the process the pid names
    /// can never be NEWER than this: a later start means reuse.
    updated: Option<f64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegStore {
    /// No `cadence.sqlite3` — provably zero rows, so "no registry row"
    /// is a proven fact, not a gap.
    Absent,
    Open,
    /// Present but not openable read-only — ownership is UNPROVEN for
    /// every tree; nothing may classify ProcessOnly.
    Unreadable,
}

impl RegStore {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Open => "open",
            Self::Unreadable => "unreadable",
        }
    }
}

struct RegEvidence {
    store: RegStore,
    agents: Vec<RegAgent>,
}

/// agents + pending/progress facts from `cadence.sqlite3`, opened
/// SQLITE_OPEN_READ_ONLY — a census must not migrate or create the
/// file on a host that never ran the daemon. Caveat: the store is
/// WAL, so a read-only connection still touches `-shm`; a store that
/// exists but can't be prepared comes back `Unreadable` and every
/// tree classifies `unknown` — fail-closed, and silent in the sense
/// that no tree row will say why beyond the store marker.
fn registry_evidence(scan: &Scan) -> RegEvidence {
    let path = scan.state_dir.join("cadence.sqlite3");
    if !path.exists() {
        return RegEvidence {
            store: RegStore::Absent,
            agents: Vec::new(),
        };
    }
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return RegEvidence {
            store: RegStore::Unreadable,
            agents: Vec::new(),
        };
    };
    let mut ev = RegEvidence {
        store: RegStore::Open,
        agents: Vec::new(),
    };
    let Ok(mut st) = conn.prepare(
        "SELECT alias, provider, endpoint_kind, generation, pid, state, \
         error, cwd, updated FROM agents ORDER BY alias",
    ) else {
        // A store whose agents table is unreadable/unmigrated is as
        // good as closed for ownership purposes — fail closed.
        ev.store = RegStore::Unreadable;
        return ev;
    };
    let mut index = std::collections::HashMap::new();
    let Ok(rows) = st.query_map([], |r| {
        Ok(RegAgent {
            alias: r.get(0)?,
            provider: r.get(1)?,
            endpoint_kind: r.get(2)?,
            generation: r.get(3)?,
            endpoint_pid: r
                .get::<_, Option<i64>>(4)?
                .and_then(|p| u32::try_from(p).ok()),
            state: r.get(5)?,
            reason: r.get(6)?,
            cwd: r.get::<_, Option<String>>(7)?,
            queued: 0,
            running: 0,
            running_generation: None,
            last_progress: r.get::<_, Option<f64>>(8)?,
            updated: r.get::<_, Option<f64>>(8)?,
        })
    }) else {
        ev.store = RegStore::Unreadable;
        return ev;
    };
    for (i, row) in rows.flatten().enumerate() {
        index.insert(row.alias.clone(), i);
        ev.agents.push(row);
    }
    if let Ok(mut st) = conn.prepare(
        "SELECT alias, state, COUNT(*) FROM messages \
         WHERE state IN ('queued','submitting','running') \
         GROUP BY alias, state",
    ) {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        }) {
            for row in rows.flatten() {
                if let Some(a) = index.get(&row.0).map(|i| &mut ev.agents[*i]) {
                    if row.1 == "running" {
                        a.running = row.2;
                    } else {
                        a.queued += row.2;
                    }
                }
            }
        }
    }
    if let Ok(mut st) = conn.prepare(
        "SELECT alias, MAX(COALESCE(completed, started, created)) \
         FROM messages GROUP BY alias",
    ) {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))
        }) {
            for (alias, at) in rows.flatten() {
                if let (Some(a), Some(at)) = (index.get(&alias).map(|i| &mut ev.agents[*i]), at) {
                    a.last_progress = Some(at.max(a.last_progress.unwrap_or(0.0)));
                }
            }
        }
    }
    // A running turn's token embeds the endpoint generation that
    // accepted it (`<kind>-<generation>-<uuid>`) — the one piece of
    // live-endpoint state readable without the daemon.
    if let Ok(mut st) = conn.prepare(
        "SELECT alias, turn_id FROM messages \
         WHERE state='running' AND turn_id IS NOT NULL",
    ) {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            for (alias, token) in rows.flatten() {
                if let Some(a) = index.get(&alias).map(|i| &mut ev.agents[*i]) {
                    // `<kind>-<generation>-<uuid>`; the generation is a
                    // simple uuid — 32 hex, no dashes. Any other token
                    // shape is not generation evidence.
                    let gen = token
                        .strip_prefix(&format!("{}-", a.endpoint_kind))
                        .and_then(|rest| rest.split('-').next())
                        .filter(|g| g.len() == 32 && g.chars().all(|c| c.is_ascii_hexdigit()));
                    if let Some(gen) = gen {
                        a.running_generation = Some(gen.to_string());
                    }
                }
            }
        }
    }
    ev
}

/// Every pid's `stat` row — the census walk. cwd/exe links are read
/// only for the handful of session roots afterwards, not per pid.
fn collect_procs(proc_root: &Path) -> (BTreeMap<u32, ProcStat>, u64, u64) {
    let mut procs = BTreeMap::new();
    let mut unreadable = 0_u64;
    let mut vanished = 0_u64;
    let Ok(pids) = std::fs::read_dir(proc_root) else {
        return (procs, unreadable, vanished);
    };
    for ent in pids.flatten() {
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        match proc_stat(&ent.path()) {
            Some(st) => {
                procs.insert(pid, st);
            }
            None if ent.path().exists() => unreadable += 1,
            None => vanished += 1,
        }
    }
    (procs, unreadable, vanished)
}

/// Does walking ppid from `pid` reach a session-family ancestor —
/// cycle-safe, bounded by the process count. A session comm with such
/// an ancestor is a member of that session's tree, not a root.
fn has_session_ancestor(pid: u32, procs: &BTreeMap<u32, ProcStat>) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut cur = procs.get(&pid).map(|p| p.ppid).unwrap_or(0);
    while cur != 0 && seen.insert(cur) {
        match procs.get(&cur) {
            None => return false, // parent gone/unreadable — chain ends
            Some(p) if SESSION_FAMILIES.contains(&comm_family(&p.comm).as_str()) => {
                return true;
            }
            Some(p) => cur = p.ppid,
        }
    }
    false
}

/// `/proc/<pid>/cwd` — read_link target, with the kernel's
/// " (deleted)" suffix split out so a dead worktree is visible.
fn proc_cwd(pid_dir: &Path) -> (Option<PathBuf>, bool) {
    match std::fs::read_link(pid_dir.join("cwd")) {
        Ok(p) => {
            let s = p.to_string_lossy();
            if let Some(live) = s.strip_suffix(" (deleted)") {
                (Some(PathBuf::from(live)), true)
            } else {
                (Some(p), false)
            }
        }
        Err(_) => (None, false),
    }
}

/// `smaps_rollup` `Pss:` in bytes — the proportional figure the audit
/// validated. `None` = absent (kernel <4.15, fixture) or denied.
fn proc_pss(pid_dir: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(pid_dir.join("smaps_rollup")).ok()?;
    let v = text.lines().find_map(|l| l.strip_prefix("Pss:"))?;
    let kb: u64 = v.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb * 1024)
}

/// `status` `VmSwap:` in bytes — the cost RSS hides (idle wrappers
/// are swapped out). A readable status without the line means the
/// kernel reports no swap for the pid — 0, not missing.
fn proc_swap(pid_dir: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(pid_dir.join("status")).ok()?;
    let v = text
        .lines()
        .find_map(|l| l.strip_prefix("VmSwap:"))
        .unwrap_or("0");
    let kb: u64 = v.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb * 1024)
}

/// `status` `Uid:` real uid — the ownership axis a foreign user's
/// session fails. `None` = status unreadable or field absent: uid
/// unproven, and unproven is never "ours".
fn proc_uid(pid_dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(pid_dir.join("status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// True when the pid lives in a different mount namespace than the
/// doctor — its `/proc/<pid>/cwd` then resolves in the target's root,
/// so a cwd string matching a registered repo is not scope proof.
/// Missing links (fixtures, restricted procfs) read as same-ns.
fn proc_ns_foreign(proc_root: &Path, pid: u32) -> bool {
    let ours = std::fs::read_link(proc_root.join("self/ns/mnt"));
    let theirs = std::fs::read_link(proc_root.join(pid.to_string()).join("ns/mnt"));
    matches!((ours, theirs), (Ok(o), Ok(t)) if o != t)
}

/// The per-pid PSS+swap pass, run over a tree's members once each —
/// partial reads are counted, totals stay measured-not-estimated.
/// Past `MAX_METRIC_PIDS` the pass stops and marks itself truncated.
fn tree_metrics(proc_root: &Path, members: &[u32]) -> TreeMetrics {
    let mut m = TreeMetrics {
        pss_bytes: 0,
        swap_bytes: 0,
        pss_missing: 0,
        swap_missing: 0,
        truncated: members.len() > MAX_METRIC_PIDS,
    };
    for pid in members.iter().take(MAX_METRIC_PIDS) {
        let dir = proc_root.join(pid.to_string());
        match proc_pss(&dir) {
            Some(b) => m.pss_bytes += b,
            None => m.pss_missing += 1,
        }
        match proc_swap(&dir) {
            Some(b) => m.swap_bytes += b,
            None => m.swap_missing += 1,
        }
    }
    m
}

/// Session roots = topmost session-family pids; each tree is the
/// transitive descendants of its root, each pid counted once — an
/// MCP wrapper lands inside its session's tree, never beside it.
fn session_trees(
    procs: &BTreeMap<u32, ProcStat>,
    proc_root: &Path,
    uptime: Option<f64>,
) -> Vec<SessionTree> {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (pid, st) in procs {
        children.entry(st.ppid).or_default().push(*pid);
    }
    let mut roots: Vec<u32> = procs
        .iter()
        .filter(|(pid, st)| {
            SESSION_FAMILIES.contains(&comm_family(&st.comm).as_str())
                && !has_session_ancestor(**pid, procs)
        })
        .map(|(pid, _)| *pid)
        .collect();
    roots.sort_unstable();
    let mut trees = Vec::new();
    for root in roots {
        // Visited set: a ppid cycle from mid-walk pid reuse must not
        // loop the DFS — `members` itself is built from `seen`.
        let mut members = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(root);
        let mut stack = vec![root];
        while let Some(pid) = stack.pop() {
            members.push(pid);
            if let Some(kids) = children.get(&pid) {
                for &kid in kids {
                    if seen.insert(kid) {
                        stack.push(kid);
                    }
                }
            }
        }
        members.sort_unstable();
        let st = &procs[&root];
        let root_dir = proc_root.join(root.to_string());
        let (cwd, cwd_deleted) = proc_cwd(&root_dir);
        let started = st.start_jiffies / hz;
        let member_rows: Vec<(u32, u64)> = members
            .iter()
            .map(|p| (*p, procs.get(p).map(|s| s.start_jiffies).unwrap_or(0)))
            .collect();
        trees.push(SessionTree {
            root_pid: root,
            root_start_jiffies: st.start_jiffies,
            root_uid: proc_uid(&root_dir),
            root_ns_foreign: proc_ns_foreign(proc_root, root),
            root_family: comm_family(&st.comm),
            root_cwd: cwd,
            root_cwd_deleted: cwd_deleted,
            root_cpu_secs: st.cpu_jiffies / hz,
            age_secs: uptime.map(|u| (u as u64).saturating_sub(started)),
            members: member_rows,
            metrics: tree_metrics(proc_root, &members),
            owner: None,
            agreement: Agreement::Unknown,
            agreement_why: String::new(),
            scope: Scope::Unproven,
        });
    }
    trees
}

/// Canonicalised `(label, path)` pairs that prove a tree's cwd is
/// inside this pm — one per registered project repo, plus the pm dir
/// itself. No pm.yaml → no scope proof → every unowned tree is
/// `unproven`, never foreign and never a candidate.
///
/// A registered path must name a directory INSIDE the host: empty or
/// relative strings are skipped (`Path::starts_with("")` is true for
/// everything), canonicalisation must succeed, and a path that
/// resolves to `/` or to the scan's home dir would scope the whole
/// host — skipped too. One bad pm.yaml line must never widen scope.
fn scope_roots(scan: &Scan) -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    let Some(pm) = &scan.pm_dir else {
        return roots;
    };
    let home = std::fs::canonicalize(&scan.home).unwrap_or_else(|_| scan.home.clone());
    let mut push = |label: String, raw: &Path| {
        let Some(canon) = (raw.is_absolute() && !raw.as_os_str().is_empty())
            .then(|| std::fs::canonicalize(raw).ok())
            .flatten()
        else {
            return;
        };
        if canon == Path::new("/") || canon == home {
            return;
        }
        roots.push((label, canon));
    };
    for project in crate::issue::project::list(pm).unwrap_or_default() {
        for repo in &project.repos {
            if let Some(path) = &repo.path {
                let expanded = crate::issue::project::expand_home(path);
                push(format!("project:{}", project.key), &expanded);
            }
        }
    }
    push("pm".to_string(), pm);
    roots
}

/// What /proc proves about one row's recorded endpoint pid. `Live`
/// is the only state that may carry ownership; the rest are stale
/// or unverifiable claims that must fence, never bind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClaimState {
    /// No endpoint pid recorded (inbox agents) — not a claim.
    None,
    /// pid alive, same uid, start bound by the row's last write.
    Live,
    /// Recorded pid is not in /proc — the endpoint is gone.
    Dead,
    /// Live pid but started AFTER the row's last write — reuse.
    Reused,
    /// Live pid owned by another uid — not this daemon's endpoint.
    ForeignUid,
    /// uid or start-time unreadable — the claim can't be proven.
    Unverifiable,
}

impl ClaimState {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none-recorded",
            Self::Live => "live",
            Self::Dead => "dead",
            Self::Reused => "reused",
            Self::ForeignUid => "foreign-uid",
            Self::Unverifiable => "unverifiable",
        }
    }
}

/// Slack between a process's wall-clock start and the row's `updated`
/// write: open records the pid and bumps `updated` in one step, and
/// later state writes only push `updated` further out — so a start
/// within the tolerance is the recorded process, one past it is not.
const CLAIM_START_TOLERANCE_SECS: f64 = 120.0;

/// Classify one row's endpoint-pid claim against live /proc. The
/// start-time agreement proof: `updated` is written when the pid is
/// recorded and on every later state write, so the recorded process
/// can never have started after it — `proc_start > updated + slack`
/// means the pid was recycled onto a different process.
fn classify_claim(
    a: &RegAgent,
    procs: &BTreeMap<u32, ProcStat>,
    proc_root: &Path,
    uptime: Option<f64>,
    now: SystemTime,
    euid: u32,
) -> ClaimState {
    let Some(pid) = a.endpoint_pid else {
        return ClaimState::None;
    };
    let Some(st) = procs.get(&pid) else {
        return ClaimState::Dead;
    };
    match proc_uid(&proc_root.join(pid.to_string())) {
        Some(u) if u != euid => return ClaimState::ForeignUid,
        None => return ClaimState::Unverifiable,
        _ => {}
    }
    let (Some(up), Some(updated)) = (uptime, a.updated) else {
        return ClaimState::Unverifiable;
    };
    let Ok(now_s) = now.duration_since(std::time::UNIX_EPOCH) else {
        return ClaimState::Unverifiable;
    };
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let start_wall = now_s.as_secs_f64() - up + st.start_jiffies as f64 / hz;
    if start_wall > updated + CLAIM_START_TOLERANCE_SECS {
        ClaimState::Reused
    } else {
        ClaimState::Live
    }
}

/// Does the tree's root cwd sit at or under a row's `agents.cwd` —
/// i.e. could this be that row's session? Both sides canonicalised;
/// an unresolvable row cwd proves no overlap.
fn cwd_overlaps(agent_cwd: Option<&String>, tree_cwd: Option<&PathBuf>) -> bool {
    let (Some(a), Some(t)) = (agent_cwd, tree_cwd) else {
        return false;
    };
    let ac = std::fs::canonicalize(a).unwrap_or_else(|_| PathBuf::from(a));
    let tc = std::fs::canonicalize(t).unwrap_or_else(|_| t.clone());
    tc.starts_with(&ac)
}

/// The three-way join — live tree ↔ agents row via the recorded
/// endpoint pid, over an UNSCOPED agents list. Only `Live` claims
/// (pid+start bound to the row's last write, same uid) may bind; a
/// pid claimed by two rows is ambiguous for both; and a row whose
/// recorded pid went stale while its cwd still covers a tree makes
/// that tree `Unknown`, never `ProcessOnly`. Ambiguity is `Unknown`,
/// not a guess.
fn join_ownership(
    trees: &mut [SessionTree],
    ev: &RegEvidence,
    procs: &BTreeMap<u32, ProcStat>,
    claims: &[ClaimState],
) {
    let mut claim: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, a) in ev.agents.iter().enumerate() {
        if let Some(p) = a.endpoint_pid {
            claim.entry(p).or_default().push(i);
        }
    }
    let mut members: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut seen_ancestor = std::collections::HashSet::new();
    for tree in trees.iter_mut() {
        // The lineage an endpoint pid may legitimately sit on: the
        // root itself (managed), an ancestor (pty pane pid is the
        // shell above the provider), or a member of the tree.
        members.clear();
        members.extend(tree.members.iter().map(|(pid, _)| *pid));
        let mut cur = procs.get(&tree.root_pid).map(|p| p.ppid).unwrap_or(0);
        seen_ancestor.clear();
        while cur != 0 && seen_ancestor.insert(cur) {
            match procs.get(&cur) {
                Some(p) => {
                    members.insert(cur);
                    cur = p.ppid;
                }
                None => break,
            }
        }
        // Claims on the lineage, split by what /proc proves about them.
        let mut live_hits: Vec<usize> = Vec::new();
        let mut stale_hits: Vec<usize> = Vec::new();
        for (pid, owners) in &claim {
            if members.contains(pid) {
                for &i in owners {
                    match claims[i] {
                        ClaimState::Live => live_hits.push(i),
                        ClaimState::None => {}
                        _ => stale_hits.push(i),
                    }
                }
            }
        }
        live_hits.sort_unstable();
        live_hits.dedup();
        stale_hits.sort_unstable();
        stale_hits.dedup();
        match (live_hits.as_slice(), stale_hits.as_slice()) {
            ([], []) => {
                // No row's recorded pid is anywhere on the lineage.
                // Before calling that ProcessOnly, fence on rows whose
                // claim went stale while their cwd still covers this
                // tree — a daemon restart/pane respawn leaves exactly
                // that shape, and the session may be theirs.
                let stale_owner = ev.agents.iter().enumerate().find(|(i, a)| {
                    !matches!(claims[*i], ClaimState::Live | ClaimState::None)
                        && cwd_overlaps(a.cwd.as_ref(), tree.root_cwd.as_ref())
                });
                match (stale_owner, &ev.store) {
                    (Some((_, a)), _) => {
                        tree.agreement = Agreement::Unknown;
                        tree.agreement_why = format!(
                            "row {} holds a stale endpoint pid under this cwd — \
                             ownership unproven",
                            a.alias
                        );
                    }
                    (None, RegStore::Open) => {
                        tree.agreement = Agreement::ProcessOnly;
                        tree.agreement_why =
                            "no agents row claims this tree (unscoped registry read)".to_string();
                    }
                    (None, RegStore::Absent) => {
                        tree.agreement = Agreement::ProcessOnly;
                        tree.agreement_why =
                            "no cadence.sqlite3 — no registry rows exist".to_string();
                    }
                    (None, RegStore::Unreadable) => {
                        tree.agreement = Agreement::Unknown;
                        tree.agreement_why =
                            "cadence.sqlite3 unreadable — ownership unproven".to_string();
                    }
                }
            }
            ([], stale) => {
                tree.agreement = Agreement::Unknown;
                let why: Vec<String> = stale
                    .iter()
                    .map(|&i| format!("{} ({})", ev.agents[i].alias, claims[i].as_str()))
                    .collect();
                tree.agreement_why = format!(
                    "recorded endpoint pid is not the live process — {}",
                    why.join(", ")
                );
            }
            ([one], []) => {
                let a = &ev.agents[*one];
                tree.owner = Some(*one);
                match (&a.generation, &a.running_generation) {
                    (Some(recorded), Some(live)) if recorded != live => {
                        tree.agreement = Agreement::GenerationMismatch;
                        tree.agreement_why = format!(
                            "registry generation {}… ≠ running turn's {}",
                            recorded.chars().take(8).collect::<String>(),
                            live.chars().take(8).collect::<String>()
                        );
                    }
                    _ => {
                        tree.agreement = Agreement::Agreed;
                        tree.agreement_why =
                            format!("endpoint pid+start claims the tree ({})", a.alias);
                    }
                }
            }
            (live, stale) => {
                tree.agreement = Agreement::Unknown;
                tree.agreement_why = format!(
                    "endpoint pid claimed by {} live + {} stale registry rows — ambiguous",
                    live.len(),
                    stale.len()
                );
            }
        }
    }
}

/// cwd → scope verdict. Both sides canonicalised. A deleted cwd only
/// names where the process stood — the directory is gone, and the
/// ` (deleted)` suffix is a string a directory can literally carry —
/// so deletion is `unproven`, not a match. A foreign mount namespace
/// makes the cwd string incomparable → `unproven`. An unreadable cwd
/// proves nothing → `unproven`, never foreign.
fn classify_scope(tree: &mut SessionTree, roots: &[(String, PathBuf)]) {
    let Some(cwd) = &tree.root_cwd else {
        tree.scope = Scope::Unproven;
        return;
    };
    if tree.root_cwd_deleted || tree.root_ns_foreign {
        tree.scope = Scope::Unproven;
        return;
    }
    let canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.clone());
    for (label, root) in roots {
        if canon.starts_with(root) {
            tree.scope = Scope::Project(label.clone());
            return;
        }
    }
    tree.scope = Scope::Foreign;
}

/// Reclaim verdict for one tree. Candidates are exactly the
/// ProcessOnly + same-uid + in-scope trees; everything else is
/// protected with the reason this census can prove. `confidence`
/// weighs age (the audit's tiers: ~3 days = high, hours = medium)
/// and accounting completeness.
fn reclaim_verdict(
    tree: &SessionTree,
    ev: &RegEvidence,
    euid: u32,
) -> (bool, Option<String>, String, String) {
    // (candidate, protected_reason, confidence, basis)
    let protected = |why: String| (false, Some(why), String::new(), String::new());
    match tree.agreement {
        Agreement::Agreed => {
            let alias = tree
                .owner
                .map(|i| ev.agents[i].alias.as_str())
                .unwrap_or("?");
            protected(format!("owned — registry row {alias}"))
        }
        Agreement::GenerationMismatch => {
            protected("generation disagreement — fenced, never a candidate".to_string())
        }
        Agreement::Unknown => protected(format!("unknown — {}", tree.agreement_why)),
        Agreement::ProcessOnly => {
            // uid before scope: a foreign user's tree inside a
            // registered repo is foreign, not a candidate — and a
            // root whose uid can't be read is unproven.
            match tree.root_uid {
                Some(u) if u != euid => {
                    return protected(format!("root owned by uid {u} — foreign user"));
                }
                None => {
                    return protected("root uid unreadable — ownership unproven".to_string());
                }
                _ => {}
            }
            match &tree.scope {
                Scope::Foreign => {
                    protected("outside this pm's project scope — foreign".to_string())
                }
                Scope::Unproven => protected(
                    "root cwd unreadable, deleted, or in another mount \
                     namespace — project scope unproven"
                        .to_string(),
                ),
                Scope::Project(_) => {
                    let mut why = Vec::new();
                    let mut confidence = match tree.age_secs {
                        Some(a) if a >= 72 * 3_600 => "high",
                        Some(a) if a >= 4 * 3_600 => "medium",
                        _ => "low",
                    };
                    match tree.age_secs {
                        Some(a) => why.push(format!("root age {}h", a / 3_600)),
                        None => {
                            confidence = "low";
                            why.push("age unproven (no /proc/uptime)".to_string());
                        }
                    }
                    if tree.metrics.truncated {
                        if confidence == "high" {
                            confidence = "medium";
                        }
                        why.push(format!(
                            "accounting truncated at {} members — totals are a lower bound",
                            MAX_METRIC_PIDS
                        ));
                    }
                    if tree.metrics.pss_missing + tree.metrics.swap_missing > 0 {
                        if confidence == "high" {
                            confidence = "medium";
                        }
                        why.push(format!(
                            "partial accounting ({} pids missing PSS, {} missing swap)",
                            tree.metrics.pss_missing, tree.metrics.swap_missing
                        ));
                    }
                    (
                        true,
                        None,
                        confidence.to_string(),
                        format!("unowned and in-scope; {}", why.join("; ")),
                    )
                }
            }
        }
    }
}

/// `doctor --host`'s owned-session-tree census — the CAD-188 phase-1
/// surface. It acts on nothing, but the level follows the evidence:
/// an unreadable registry is an evidence failure (every tree becomes
/// `unknown`, and a watchdog keying on the exit code must not see 0),
/// and a live candidate list warns so `render` actually prints the
/// authorisation remedy — remedies render only for non-ok levels, so
/// a constant `Ok` would ship a caveat that cannot print. `session
/// start`/`end` map warn to exit 1 ("GO with warnings") — honest for
/// both cases: a human's `claude` in the repo stays visible, and a
/// broken registry is loud.
fn check_sessions(scan: &Scan) -> Check {
    let name = "sessions";
    let threshold = json!(
        "warn: registry unreadable, or unowned in-scope session trees present \
         (dry-run census — never an action)"
    );
    if !scan.linux {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "session census is linux-only".to_string(),
            String::new(),
        );
    }
    let ev = registry_evidence(scan);
    let (procs, unreadable, vanished) = collect_procs(&scan.proc_root);
    let uptime = proc_uptime(&scan.proc_root);
    let claims: Vec<ClaimState> = ev
        .agents
        .iter()
        .map(|a| classify_claim(a, &procs, &scan.proc_root, uptime, scan.now, scan.uid))
        .collect();
    let mut trees = session_trees(&procs, &scan.proc_root, uptime);
    join_ownership(&mut trees, &ev, &procs, &claims);
    let roots = scope_roots(scan);
    for tree in &mut trees {
        classify_scope(tree, &roots);
    }
    // Registry rows no live tree claimed — the record-only class:
    // rows with a dead endpoint pid, or none at all (inbox). Still
    // listed so the census shows the whole registry it read, with
    // what /proc proved about each recorded endpoint pid.
    let records_only: Vec<Value> = ev
        .agents
        .iter()
        .enumerate()
        .filter(|(i, _)| !trees.iter().any(|t| t.owner == Some(*i)))
        .map(|(i, a)| {
            json!({
                "alias": a.alias,
                "provider": a.provider,
                "endpoint_kind": a.endpoint_kind,
                "state": a.state,
                "reason": a.reason,
                "endpoint_pid": a.endpoint_pid,
                "endpoint_state": claims[i].as_str(),
                "cwd": a.cwd,
                "queued": a.queued,
                "running": a.running,
                "last_progress": a.last_progress,
                "agreement": "record-only",
            })
        })
        .collect();
    let mut candidates: Vec<(u32, String)> = Vec::new();
    let mut rows = Vec::new();
    let mut cand_pss = 0_u64;
    let mut cand_swap = 0_u64;
    let mut cand_truncated = false;
    for tree in &trees {
        let (candidate, protected, confidence, basis) = reclaim_verdict(tree, &ev, scan.uid);
        if candidate {
            cand_truncated |= tree.metrics.truncated;
            // The detail line is what a human pastes into a terminal —
            // carry the context a bare pid lacks.
            candidates.push((
                tree.root_pid,
                format!(
                    "{}({}, {}, uid={}, {}h)",
                    tree.root_pid,
                    tree.root_family,
                    tree.scope.as_str(),
                    tree.root_uid
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| "?".to_string()),
                    tree.age_secs.unwrap_or(0) / 3_600
                ),
            ));
            cand_pss += tree.metrics.pss_bytes;
            cand_swap += tree.metrics.swap_bytes;
        }
        let owner = tree.owner.map(|i| &ev.agents[i]);
        rows.push(json!({
            "root": {
                "pid": tree.root_pid,
                "start_jiffies": tree.root_start_jiffies,
                "uid": tree.root_uid,
                "ns_foreign": tree.root_ns_foreign,
                "family": tree.root_family,
                "cwd": tree.root_cwd,
                "cwd_deleted": tree.root_cwd_deleted,
                "age_secs": tree.age_secs,
                "cpu_secs": tree.root_cpu_secs,
            },
            "alias": owner.map(|a| a.alias.as_str()),
            "endpoint_kind": owner.map(|a| a.endpoint_kind.as_str()),
            "generation": owner.and_then(|a| a.generation.as_deref()),
            "endpoint_pid": owner.and_then(|a| a.endpoint_pid),
            "state": owner.map(|a| a.state.as_str()),
            "reason": owner.and_then(|a| a.reason.as_deref()),
            "pending": owner.map(|a| json!({"queued": a.queued, "running": a.running})),
            "last_progress": owner.and_then(|a| a.last_progress),
            "agreement": tree.agreement.as_str(),
            "agreement_why": tree.agreement_why,
            "scope": tree.scope.as_str(),
            "procs": tree.members.len(),
            "members": tree
                .members
                .iter()
                .map(|(pid, start)| json!({"pid": pid, "start_jiffies": start}))
                .collect::<Vec<_>>(),
            "pss_bytes": tree.metrics.pss_bytes,
            "swap_bytes": tree.metrics.swap_bytes,
            "pss_missing_pids": tree.metrics.pss_missing,
            "swap_missing_pids": tree.metrics.swap_missing,
            "metrics_truncated": tree.metrics.truncated,
            "reclaim": {
                "candidate": candidate,
                "pss_bytes": tree.metrics.pss_bytes,
                "swap_bytes": tree.metrics.swap_bytes,
                "confidence": confidence,
                "basis": basis,
                "protected": protected,
            },
        }));
    }
    let owned = trees
        .iter()
        .filter(|t| t.agreement == Agreement::Agreed)
        .count();
    let process_only = trees
        .iter()
        .filter(|t| t.agreement == Agreement::ProcessOnly)
        .count();
    let unowned_in_scope = trees
        .iter()
        .filter(|t| t.agreement == Agreement::ProcessOnly && matches!(t.scope, Scope::Project(_)))
        .count();
    let unowned_foreign = trees
        .iter()
        .filter(|t| t.agreement == Agreement::ProcessOnly && t.scope == Scope::Foreign)
        .count();
    let unowned_unproven = trees
        .iter()
        .filter(|t| t.agreement == Agreement::ProcessOnly && t.scope == Scope::Unproven)
        .count();
    let mismatched = trees
        .iter()
        .filter(|t| t.agreement == Agreement::GenerationMismatch)
        .count();
    let uncertain = trees
        .iter()
        .filter(|t| t.agreement == Agreement::Unknown)
        .count();
    let foreign_uid = trees
        .iter()
        .filter(|t| {
            t.agreement == Agreement::ProcessOnly && t.root_uid.is_some_and(|u| u != scan.uid)
        })
        .count();
    let registry_scope = match ev.store {
        RegStore::Open => format!("unscoped — all {} agents rows", ev.agents.len()),
        RegStore::Absent => "unscoped — store absent (zero rows)".to_string(),
        RegStore::Unreadable => "unscoped read FAILED — store unreadable".to_string(),
    };
    let mut detail = format!(
        "{} trees: {} owned, {} unowned ({} in-scope / {} foreign / {} unproven / \
         {} foreign-uid), {} generation-mismatch, {} uncertain; registry {} rows ({})",
        trees.len(),
        owned,
        process_only,
        unowned_in_scope,
        unowned_foreign,
        unowned_unproven,
        foreign_uid,
        mismatched,
        uncertain,
        ev.agents.len(),
        ev.store.as_str(),
    );
    if !candidates.is_empty() {
        // PSS shares can overlap across candidate trees, so the sum
        // is normally an upper bound — but a truncated tree's totals
        // are a lower bound over its first members, and one of those
        // in the list makes the aggregate neither: say so.
        if cand_truncated {
            detail.push_str(&format!(
                "; candidates (dry-run) would free ~{} RAM + ~{} swap \
                 (estimate — PSS overlap and a truncated tree)",
                human(cand_pss),
                human(cand_swap)
            ));
        } else {
            detail.push_str(&format!(
                "; candidates (dry-run) would free ≤{} RAM + ≤{} swap (upper bound)",
                human(cand_pss),
                human(cand_swap)
            ));
        }
        detail.push_str(&format!(
            "; candidates: {}",
            candidates
                .iter()
                .take(8)
                .map(|(_, d)| d.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ));
        // Inlined into detail because render() only prints `remedy`
        // for non-ok checks and candidates stay ok — the caveat must
        // reach the operator next to the pid list it covers.
        detail.push_str(
            "; dry-run — nothing is stopped; any cleanup needs a separately \
             authorised phase with an ownership recheck at action time",
        );
    }
    if unreadable + vanished > 0 {
        detail.push_str(&format!(
            "; {} pids unreadable, {} vanished mid-scan",
            unreadable, vanished
        ));
    }
    let value = json!({
        "registry_scope": registry_scope,
        "registry_agents": ev.agents.len(),
        "store": ev.store.as_str(),
        "trees": rows,
        "records_only": records_only,
        "candidates": candidates
            .iter()
            .map(|(pid, _)| *pid)
            .collect::<Vec<_>>(),
        "totals": {
            "trees": trees.len(),
            "owned": owned,
            "process_only": process_only,
            "unowned_in_scope": unowned_in_scope,
            "unowned_foreign": unowned_foreign,
            "unowned_unproven": unowned_unproven,
            "foreign_uid": foreign_uid,
            "generation_mismatch": mismatched,
            "uncertain": uncertain,
            "candidates": candidates.len(),
            "candidate_pss_bytes": cand_pss,
            "candidate_swap_bytes": cand_swap,
            // False the moment any candidate's metrics were truncated
            // — a lower-bound tree in the sum means the aggregate is
            // an estimate, not an upper bound.
            "candidate_sums_upper_bound": !cand_truncated,
        },
        "procs_scanned": procs.len(),
        "pids_unreadable": unreadable,
        "pids_vanished": vanished,
    });
    let remedy = if candidates.is_empty() {
        String::new()
    } else {
        "dry-run census — nothing is stopped or reaped; per-tree rows are under \
         checks.sessions.trees in --json. Any cleanup needs a separately authorised \
         phase with an ownership recheck at action time."
            .to_string()
    };
    // Warn only on evidence failure: an unreadable store means the
    // census could not classify, and a watchdog must see that. A live
    // candidate list is a routine condition (a human ran an agent in a
    // repo) and stays ok — its caveat is inlined in `detail` above.
    let level = if ev.store == RegStore::Unreadable {
        Level::Warn
    } else {
        Level::Ok
    };
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

// ---------- legacy task cargo targets ----------
//
// `temp-dirs` only matches `cadence-` / `.tmp` / `tmp.`. Per-task
// cargo output such as `/tmp/cad156-fix-target` is invisible there,
// and a name that looks similar is not proof the directory is ours
// or that it is idle. This check inventories that namespace and
// stops. It does not join `--reclaim-plan` and it never emits a
// deletion command: live, locked, foreign, symlink, name-only and
// merely unproven rows are all excluded, and "no cwd/exe pointed
// here" is reported as unproven rather than safe.

/// `cad156-fix-target`, `cad173-nextest-target-one`,
/// `cad176-pr100-target`. The `cad` + digits + `-` head is the
/// legacy task prefix; `target` anywhere in the tail is what keeps
/// `cad156-fix` and `cadence-*` out. Byte-wise so a non-UTF8 temp
/// name cannot be lossily rewritten into a match.
fn legacy_task_target_name(name: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let bytes = name.as_bytes();
    let Some(rest) = bytes.strip_prefix(b"cad") else {
        return false;
    };
    let Some(dash) = rest.iter().position(|b| *b == b'-') else {
        return false;
    };
    let num = &rest[..dash];
    let tail = &rest[dash + 1..];
    !num.is_empty()
        && num.iter().all(|b| b.is_ascii_digit())
        && tail.windows(6).any(|w| w == b"target")
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn normalize_abs(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    let path = if trimmed.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(trimmed)
    };
    Some(lexical_normalize(&path))
}

fn under_cadence_tree(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".cadence")
}

fn configured_cargo_target(scan: &Scan) -> Option<PathBuf> {
    let raw = scan.cargo_target_dir.as_ref()?;
    let joined = if raw.is_absolute() {
        raw.clone()
    } else {
        scan.cwd.join(raw)
    };
    normalize_abs(&joined)
}

/// Keep the stronger gap. `unreadable` must not collapse back to
/// `incomplete` when a later entry fails a milder check.
fn note_record(status: &mut &'static str, next: &'static str) {
    fn rank(s: &str) -> u8 {
        match s {
            "unreadable" => 3,
            "incomplete" => 2,
            _ => 0,
        }
    }
    if rank(next) > rank(status) {
        *status = next;
    }
}

/// Issue ids whose worktree ref records this cargo target. Symlinked
/// issue files and project dirs are skipped rather than followed; a
/// skip or a read error makes the search status incomplete so a
/// missing record is not treated as proof of non-ownership.
fn recorded_cargo_targets(pm: Option<&Path>) -> (BTreeMap<PathBuf, Vec<String>>, &'static str) {
    let mut map: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let Some(pm) = pm else {
        return (map, "no-tracker");
    };
    let Ok(meta) = std::fs::symlink_metadata(pm) else {
        return (map, "unreadable");
    };
    if !meta.is_dir() {
        return (map, "unreadable");
    }
    let Ok(projects) = std::fs::read_dir(pm) else {
        return (map, "unreadable");
    };
    let mut status = "complete";
    let mut seen = 0_usize;
    for project in projects {
        let project = match project {
            Ok(project) => project,
            Err(_) => {
                note_record(&mut status, "incomplete");
                continue;
            }
        };
        let Ok(kind) = project.file_type() else {
            note_record(&mut status, "incomplete");
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let Ok(issues) = std::fs::read_dir(project.path()) else {
            note_record(&mut status, "unreadable");
            continue;
        };
        for issue in issues {
            if seen >= TASK_TARGET_ISSUE_BUDGET {
                return (map, "truncated");
            }
            let issue = match issue {
                Ok(issue) => issue,
                Err(_) => {
                    note_record(&mut status, "incomplete");
                    continue;
                }
            };
            let Ok(kind) = issue.file_type() else {
                note_record(&mut status, "incomplete");
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let file = issue.path().join("issue.md");
            let meta = match std::fs::symlink_metadata(&file) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    note_record(&mut status, "incomplete");
                    continue;
                }
            };
            if meta.file_type().is_symlink() {
                note_record(&mut status, "incomplete");
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            seen += 1;
            if meta.len() > 1_048_576 {
                note_record(&mut status, "incomplete");
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                note_record(&mut status, "incomplete");
                continue;
            };
            let Ok((front, _)) = crate::issue::parse::parse_issue(&text) else {
                note_record(&mut status, "incomplete");
                continue;
            };
            for r in &front.refs {
                if r.kind != "worktree" {
                    continue;
                }
                let Some(raw) = r.cargo_target.as_deref() else {
                    continue;
                };
                let Some(path) = normalize_abs(Path::new(raw)) else {
                    note_record(&mut status, "incomplete");
                    continue;
                };
                let ids = map.entry(path).or_default();
                if !ids.iter().any(|id| id == &front.id) {
                    ids.push(front.id.clone());
                    ids.sort();
                }
            }
        }
    }
    (map, status)
}

#[derive(Debug, PartialEq, Eq)]
enum LockBit {
    Absent,
    Free,
    Held,
    Unknown,
}

/// What `lstat` can say about `path` without crossing a symlink.
/// `symlink_metadata` on the full path still walks ancestor links, so
/// `/tmp/link/target` looks like a real directory when `link` is a
/// symlink. Each component is `lstat`'d on its own and the walk stops
/// at the first link.
enum LexicalKind {
    Absent,
    /// `at` is the symlink component. `final_component` is false when
    /// an ancestor, not the path itself, is the link.
    Symlink {
        at: PathBuf,
        final_component: bool,
    },
    Ready(std::fs::Metadata),
    Error,
}

fn lexical_kind(path: &Path) -> LexicalKind {
    if !path.is_absolute() {
        return LexicalKind::Error;
    }
    let mut cur = PathBuf::new();
    let comps: Vec<_> = path.components().collect();
    let last = comps.len().saturating_sub(1);
    for (i, c) in comps.into_iter().enumerate() {
        match c {
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                cur.push(c);
            }
            std::path::Component::CurDir | std::path::Component::ParentDir => {
                return LexicalKind::Error;
            }
            std::path::Component::Normal(name) => {
                cur.push(name);
                match std::fs::symlink_metadata(&cur) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        return LexicalKind::Symlink {
                            at: cur,
                            final_component: i == last,
                        };
                    }
                    Ok(meta) if i == last => return LexicalKind::Ready(meta),
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => return LexicalKind::Error,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return LexicalKind::Absent;
                    }
                    Err(_) => return LexicalKind::Error,
                }
            }
        }
    }
    LexicalKind::Error
}

/// Non-blocking exclusive probe of one cargo lock file.
///
/// A FIFO named `.cargo-lock` blocks `open` forever. A final-component
/// symlink is not the only trap: `lstat` of the basename still follows
/// ancestor links, and a replacement between the check and `open` can
/// swap in a FIFO or a symlink. Non-regular files are rejected first.
/// The open itself is one `O_NOFOLLOW | O_NONBLOCK` call, and the fd is
/// kept only when `fstat` still says it is a regular file.
fn probe_lock_file(path: &Path) -> LockBit {
    match lexical_kind(path) {
        LexicalKind::Absent => return LockBit::Absent,
        LexicalKind::Ready(meta) if meta.is_file() => {}
        LexicalKind::Ready(_) | LexicalKind::Symlink { .. } | LexicalKind::Error => {
            return LockBit::Unknown;
        }
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LockBit::Absent,
        Err(_) => return LockBit::Unknown,
    };
    match file.metadata() {
        Ok(meta) if meta.is_file() => {}
        _ => return LockBit::Unknown,
    }
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return LockBit::Free;
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        LockBit::Held
    } else {
        LockBit::Unknown
    }
}

fn fold_lock(status: &'static str, bit: LockBit) -> &'static str {
    match (status, bit) {
        (_, LockBit::Held) | ("held", _) => "held",
        ("unknown", _) | (_, LockBit::Unknown) => "unknown",
        (_, LockBit::Free) => "free",
        (status, LockBit::Absent) => status,
    }
}

/// Cargo's build locks at the target root and under `debug/` /
/// `release/` only. A symlinked profile directory is not entered,
/// and neither is a directory reached through an ancestor symlink.
fn cargo_lock_status(dir: &Path) -> &'static str {
    match lexical_kind(dir) {
        LexicalKind::Ready(meta) if meta.is_dir() => {}
        _ => return "unknown",
    }
    let mut status = "absent";
    for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
        status = fold_lock(status, probe_lock_file(&dir.join(name)));
        if status == "held" {
            return status;
        }
    }
    for sub in ["debug", "release"] {
        let subdir = dir.join(sub);
        match std::fs::symlink_metadata(&subdir) {
            Ok(m) if m.file_type().is_symlink() => {
                status = fold_lock(status, LockBit::Unknown);
            }
            Ok(m) if m.is_dir() => {
                for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
                    status = fold_lock(status, probe_lock_file(&subdir.join(name)));
                    if status == "held" {
                        return status;
                    }
                }
            }
            _ => {}
        }
    }
    status
}

struct ProcSeen {
    pids: Vec<u32>,
    extra: usize,
}

/// One pass over `proc_root`. Cwd and exe are `read_link` results —
/// the link text, not a followed target. `partial` means a pid denied
/// both links or a directory entry could not be read, so a row with
/// no hit is not proof that nothing references it.
fn task_target_proc_hits(proc_root: &Path, roots: &[PathBuf]) -> (Vec<ProcSeen>, &'static str) {
    let mut hits = roots
        .iter()
        .map(|_| ProcSeen {
            pids: Vec::new(),
            extra: 0,
        })
        .collect::<Vec<_>>();
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return (hits, "unreadable");
    };
    let mut seen = 0_usize;
    let mut partial = false;
    for ent in entries {
        let ent = match ent {
            Ok(ent) => ent,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        if seen >= TASK_TARGET_PROC_BUDGET {
            return (hits, "truncated");
        }
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        seen += 1;
        let cwd = std::fs::read_link(ent.path().join("cwd"));
        let exe = std::fs::read_link(ent.path().join("exe"));
        if cwd
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
            && exe
                .as_ref()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        {
            partial = true;
            continue;
        }
        for (i, root) in roots.iter().enumerate() {
            let matched = [cwd.as_ref(), exe.as_ref()]
                .into_iter()
                .flatten()
                .any(|link| {
                    let text = link.to_string_lossy();
                    let stripped = text.strip_suffix(" (deleted)").unwrap_or(text.as_ref());
                    let path = lexical_normalize(Path::new(stripped));
                    path == *root || path.starts_with(root)
                });
            if !matched {
                continue;
            }
            let hit = &mut hits[i];
            if hit.pids.len() < TASK_TARGET_PIDS {
                if !hit.pids.contains(&pid) {
                    hit.pids.push(pid);
                }
            } else {
                hit.extra += 1;
            }
        }
    }
    for hit in &mut hits {
        hit.pids.sort_unstable();
    }
    let status = if partial { "partial" } else { "complete" };
    (hits, status)
}

struct TaskCandidate {
    path: PathBuf,
    name_match: bool,
}

fn check_task_targets(scan: &Scan) -> Check {
    let name = "task-targets";
    let t = &scan.thresholds;
    let threshold = json!(format!(
        "warn: ≥{} pressure dirs, ≥{}, a truncated or incomplete temp/tracker/proc scan, or any active/locked/unknown row — inventory only, never a deletion list",
        t.temp_warn_count,
        human(t.temp_warn_bytes)
    ));
    let (recorded, record_search) = recorded_cargo_targets(scan.pm_dir.as_deref());
    let configured = configured_cargo_target(scan);

    let mut candidates: BTreeMap<PathBuf, bool> = BTreeMap::new();
    let mut temp_scan = "complete";
    match std::fs::read_dir(&scan.temp_dir) {
        Ok(entries) => {
            let mut n = 0_usize;
            for ent in entries {
                if n >= TASK_TARGET_TEMP_BUDGET {
                    temp_scan = "truncated";
                    break;
                }
                let ent = match ent {
                    Ok(ent) => ent,
                    Err(_) => {
                        if temp_scan == "complete" {
                            temp_scan = "incomplete";
                        }
                        continue;
                    }
                };
                n += 1;
                if !legacy_task_target_name(&ent.file_name()) {
                    continue;
                }
                // `file_type` does not follow links. A symlink is
                // inventoried and not walked; a regular file is not a
                // cargo target directory.
                let Ok(kind) = ent.file_type() else {
                    temp_scan = "incomplete";
                    continue;
                };
                if !kind.is_dir() && !kind.is_symlink() {
                    continue;
                }
                let path = lexical_normalize(&ent.path());
                candidates.insert(path, true);
            }
        }
        Err(_) => temp_scan = "unreadable",
    }
    for path in recorded.keys() {
        candidates.entry(path.clone()).or_insert(false);
    }
    if let Some(path) = &configured {
        candidates.entry(path.clone()).or_insert(false);
    }

    let mut scan_truncated = temp_scan == "truncated" || record_search == "truncated";
    let mut selected: Vec<TaskCandidate> = candidates
        .into_iter()
        .map(|(path, name_match)| TaskCandidate { path, name_match })
        .collect();
    if selected.len() > TASK_TARGET_ROW_CAP {
        selected.truncate(TASK_TARGET_ROW_CAP);
        scan_truncated = true;
    }

    let roots: Vec<PathBuf> = selected.iter().map(|c| c.path.clone()).collect();
    let (hits, proc_scan) = task_target_proc_hits(&scan.proc_root, &roots);

    let pressure_slots = selected
        .iter()
        .filter(|c| !under_cadence_tree(&c.path))
        .count()
        .max(1);
    // Every legacy directory gets the same slice of the stat budget.
    // Walking recorded `.cadence` targets first would consume it and
    // leave the `/tmp/cad*-target*` rows unsized.
    let per_dir_budget =
        (TASK_TARGET_STAT_BUDGET / pressure_slots).clamp(1, TASK_TARGET_DIR_STAT_CAP);
    let mut rows: Vec<Value> = Vec::new();
    let mut pressure_count = 0_u64;
    let mut pressure_bytes = 0_u64;
    let record_gap = !matches!(record_search, "complete" | "no-tracker");
    // An unknown proc/tracker/temp scan must not leave the check `ok`.
    // `run` takes the worst level and `exit_code` treats `ok` as a
    // healthy host.
    let proc_gap = proc_scan != "complete";
    let mut attention = temp_scan != "complete" || scan_truncated || record_gap || proc_gap;
    for (cand, hit) in selected.iter().zip(hits.iter()) {
        let path = &cand.path;
        let issues = recorded.get(path).cloned().unwrap_or_default();
        let is_configured = configured.as_ref().is_some_and(|p| p == path);
        let name_match = cand.name_match || path.file_name().is_some_and(legacy_task_target_name);
        let ownership = if !issues.is_empty() {
            "recorded"
        } else if is_configured {
            "configured"
        } else {
            "name-only"
        };
        let proven = ownership != "name-only";
        let pressure = !under_cadence_tree(path);
        let quoted = shell_quote(&path.display().to_string());

        // Component-wise lstat. `symlink_metadata(path)` would follow
        // an ancestor and report the final directory as real.
        let looked = lexical_kind(path);
        let ancestor_symlink = matches!(
            looked,
            LexicalKind::Symlink {
                final_component: false,
                ..
            }
        );
        let link_at = match &looked {
            LexicalKind::Symlink { at, .. } => Some(at.clone()),
            _ => None,
        };
        let symlink = link_at.is_some();
        let meta = match &looked {
            LexicalKind::Ready(m) => Some(m.clone()),
            LexicalKind::Symlink { at, .. } => std::fs::symlink_metadata(at).ok(),
            _ => None,
        };
        let exists = matches!(looked, LexicalKind::Ready(_) | LexicalKind::Symlink { .. });
        let uid = meta.as_ref().map(|m| m.uid());
        let uid_matches = uid.is_some_and(|u| u == scan.uid);
        let age_secs = meta.as_ref().and_then(|m| {
            m.modified().ok().map(|modified| {
                scan.now
                    .duration_since(modified)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            })
        });
        let link_target = link_at
            .as_deref()
            .and_then(|at| std::fs::read_link(at).ok())
            .map(|t| t.display().to_string());
        let foreign_symlink = if let Some(at) = &link_at {
            match &link_target {
                Some(target) => {
                    let target = PathBuf::from(target);
                    let base = at.parent().unwrap_or(Path::new("/"));
                    let resolved = if target.is_absolute() {
                        lexical_normalize(&target)
                    } else {
                        lexical_normalize(&base.join(target))
                    };
                    let tmp = lexical_normalize(&scan.temp_dir);
                    !resolved.starts_with(&tmp)
                }
                None => true,
            }
        } else {
            false
        };

        let (bytes, bytes_truncated, bytes_skipped, cargo_lock) = if symlink {
            (None, false, Some("symlink"), "unknown")
        } else if matches!(looked, LexicalKind::Error) {
            (None, false, Some("unreadable"), "unknown")
        } else if !exists {
            (None, false, Some("absent"), "absent")
        } else if !uid_matches {
            (None, false, Some("foreign-uid"), "unknown")
        } else if meta.as_ref().is_some_and(|m| !m.is_dir()) {
            (None, false, Some("not-a-directory"), "unknown")
        } else if !pressure {
            // The worktree check already measures these trees.
            (
                None,
                false,
                Some("tracked-elsewhere"),
                cargo_lock_status(path),
            )
        } else {
            let (b, truncated, _) = dir_size_limited(path, per_dir_budget);
            if truncated {
                scan_truncated = true;
            }
            (Some(b), truncated, None, cargo_lock_status(path))
        };

        let observed = !hit.pids.is_empty();
        // Hits we did see stay observed. A row with no hit is
        // `none-observed` only when the proc scan finished. Partial,
        // truncated, and unreadable scans are unknown negatives.
        let cwd_exe = if observed {
            "observed"
        } else if proc_scan != "complete" {
            "unreadable"
        } else {
            "none-observed"
        };
        let activity = if observed || cargo_lock == "held" {
            "active"
        } else if symlink
            || !exists
            || !uid_matches
            || cwd_exe == "unreadable"
            || cargo_lock == "unknown"
        {
            "unknown"
        } else {
            "unproven"
        };
        let exclude = if symlink {
            "symlink"
        } else if exists && !uid_matches {
            "foreign-uid"
        } else if activity == "active" {
            "active"
        } else if activity == "unknown" {
            "unknown"
        } else if ownership == "name-only" {
            "name-only"
        } else {
            "unproven"
        };

        if pressure && exists {
            pressure_count += 1;
            pressure_bytes = pressure_bytes.saturating_add(bytes.unwrap_or(0));
            if bytes_truncated
                || activity == "active"
                || activity == "unknown"
                || cargo_lock == "held"
                || cargo_lock == "unknown"
            {
                attention = true;
            }
        }

        rows.push(json!({
            "path": path,
            "quoted": quoted,
            "name": path.file_name().map(|n| n.to_string_lossy().into_owned()),
            "name_match": name_match,
            "ownership": ownership,
            "proven": proven,
            "configured": is_configured,
            "issues": issues,
            "owner_uid": uid,
            "uid_matches": exists && uid_matches,
            "age_secs": age_secs,
            "bytes": bytes,
            "bytes_truncated": bytes_truncated,
            "bytes_skipped": bytes_skipped,
            "exists": exists,
            "symlink": symlink,
            "ancestor_symlink": ancestor_symlink,
            // True only if a symlink component was crossed. Nothing in
            // this check crosses one, including an ancestor of the
            // final path, so this stays false.
            "followed": false,
            "foreign_symlink": foreign_symlink,
            "cargo_lock": cargo_lock,
            "activity": activity,
            "cwd_exe": cwd_exe,
            "pids": hit.pids,
            "pids_omitted": hit.extra,
            "pressure": pressure,
            "reclaim_candidate": false,
            "safe_to_delete": false,
            "action": "none",
            "exclude": exclude,
        }));
    }

    if scan_truncated {
        attention = true;
    }
    let level = if attention
        || pressure_count >= t.temp_warn_count
        || pressure_bytes >= t.temp_warn_bytes
    {
        Level::Warn
    } else {
        Level::Ok
    };

    let shown_rows: Vec<&Value> = rows
        .iter()
        .filter(|r| r["pressure"] == json!(true))
        .chain(rows.iter().filter(|r| r["pressure"] != json!(true)))
        .collect();
    let detail = if rows.is_empty() {
        if temp_scan == "unreadable" {
            format!(
                "temp dir {} unreadable — task targets not inventoried",
                scan.temp_dir.display()
            )
        } else if record_gap || temp_scan != "complete" || proc_scan != "complete" {
            format!(
                "none listed — temp {temp_scan}, tracker {record_search}, proc {proc_scan}; not a conclusive absence"
            )
        } else {
            "none".to_string()
        }
    } else {
        let name_only = rows
            .iter()
            .filter(|r| r["ownership"] == "name-only")
            .count();
        let proven_n = rows.iter().filter(|r| r["proven"] == json!(true)).count();
        let active = rows.iter().filter(|r| r["activity"] == "active").count();
        let unproven = rows.iter().filter(|r| r["activity"] == "unproven").count();
        let locked = rows.iter().filter(|r| r["cargo_lock"] == "held").count();
        let size = if rows.iter().any(|r| r["bytes_truncated"] == json!(true)) || scan_truncated {
            format!("at least {}", human(pressure_bytes))
        } else {
            human(pressure_bytes)
        };
        let shown = shown_rows
            .iter()
            .take(5)
            .map(|r| r["quoted"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
            .join(" ");
        let more = if rows.len() > 5 {
            format!(" (+{} more)", rows.len() - 5)
        } else {
            String::new()
        };
        let idle_note = if unproven > 0 || proc_scan != "complete" {
            " — none-observed is not proof the directory is idle"
        } else {
            ""
        };
        format!(
            "{n} task targets, {size} pressure ({name_only} name-only, {proven_n} proven; {active} active, {locked} cargo-locked, {unproven} cwd/exe none-observed{idle_note}; proc {proc_scan}){more}: {shown}",
            n = rows.len(),
        )
    };
    let remedy = if rows.is_empty() {
        String::new()
    } else {
        format!(
            "read-only inventory — nothing is deleted. A matching name is not Cadence ownership. No cwd/exe reference is not proof the directory is idle. Recheck ownership, process cwd/exe, and cargo locks immediately before any authorised reclaim. {}",
            shown_rows
                .iter()
                .take(5)
                .map(|r| format!(
                    "{} ({}, {}, lock {})",
                    r["quoted"].as_str().unwrap_or("?"),
                    r["ownership"].as_str().unwrap_or("?"),
                    r["activity"].as_str().unwrap_or("?"),
                    r["cargo_lock"].as_str().unwrap_or("?")
                ))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let value = json!({
        "count": rows.len(),
        "bytes": pressure_bytes,
        "pressure_count": pressure_count,
        "scan_truncated": scan_truncated,
        "temp_scan": temp_scan,
        "record_search": record_search,
        "proc_scan": proc_scan,
        "safe_to_delete": false,
        "record_conclusive": record_search == "complete",
        "note": "A matching name is not Cadence ownership. No observed cwd/exe reference is not proof the directory is idle. Recheck ownership, process cwd/exe, and cargo locks immediately before any authorised reclaim.",
        "rows": rows,
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

/// The load warn line when `[host] load_warn_ratio` is unset: the
/// slot plan's own ceiling plus headroom — the farm is *meant* to run
/// `(build_slots + suite_slots) × jobs_per_lane` deep, so warn above
/// 1.25× that plan (never below plain saturation). The daemon's
/// resolved config rides `scan.slots`; unreachable, the built-in
/// defaults stand in.
fn planned_load_warn_ratio(scan: &Scan, cpus: f64) -> f64 {
    let cfg = scan.slots.as_ref().map(|s| &s["config"]);
    let key = |k: &str, d: f64| cfg.and_then(|c| c[k].as_f64()).unwrap_or(d);
    let planned_jobs =
        (key("build_slots", 3.0) + key("suite_slots", 1.0)) * key("jobs_per_lane", 4.0);
    (planned_jobs * 1.25 / cpus).max(1.0)
}

/// Host pressure: load1 vs cpu count plus io stall, with the slot
/// queue in the detail so a hot host names its cause. Everything
/// reads `scan.proc_root`, so tests fabricate both files.
/// `pm.yaml [host]` itself: ok when absent or applied, warn naming the
/// error when it could not be applied (defaults in force, WAL watch off).
fn check_config(scan: &Scan) -> Check {
    let (level, detail, remedy) = match &scan.thresholds.config_error {
        None => (
            Level::Ok,
            "host thresholds applied".to_string(),
            String::new(),
        ),
        Some(e) => (
            Level::Warn,
            format!("{e} — every threshold is at its default and the WAL checkpoint watch is off"),
            "fix the [host] table in pm.yaml (unknown keys and bad values are refused)".to_string(),
        ),
    };
    Check {
        name: "config",
        level,
        value: json!({"error": scan.thresholds.config_error}),
        threshold: Value::Null,
        detail,
        remedy,
    }
}

fn check_load(scan: &Scan) -> Check {
    let name = "load";
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as f64;
    let warn_ratio = scan
        .thresholds
        .load_warn_ratio
        .unwrap_or_else(|| planned_load_warn_ratio(scan, cpus));
    let threshold = json!({
        "load1": format!("warn > {}x cpus, fail > {}x",
                         warn_ratio,
                         warn_ratio * 2.0),
        "io_stall_avg10_pct": format!("warn > {}, fail > {}",
                                     scan.thresholds.io_stall_warn_pct,
                                     scan.thresholds.io_stall_fail_pct),
    });
    let load1 = std::fs::read_to_string(scan.proc_root.join("loadavg"))
        .ok()
        .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok());
    let io_stall = std::fs::read_to_string(scan.proc_root.join("pressure/io"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("some"))?
                .split_whitespace()
                .find_map(|f| f.strip_prefix("avg10="))?
                .parse::<f64>()
                .ok()
        });
    if load1.is_none() && io_stall.is_none() {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            format!(
                "no loadavg or pressure/io under {}",
                scan.proc_root.display()
            ),
            String::new(),
        );
    }
    let ratio = load1.map(|l| l / cpus);
    let level = if ratio.is_some_and(|r| r > warn_ratio * 2.0)
        || io_stall.is_some_and(|s| s > scan.thresholds.io_stall_fail_pct)
    {
        Level::Fail
    } else if ratio.is_some_and(|r| r > warn_ratio)
        || io_stall.is_some_and(|s| s > scan.thresholds.io_stall_warn_pct)
    {
        Level::Warn
    } else {
        Level::Ok
    };
    let slots_text = match &scan.slots {
        Some(s) => {
            let held = |pool: &str| s["pools"][pool]["held"].as_array().map_or(0, Vec::len);
            let cap = |pool: &str| s["pools"][pool]["capacity"].as_u64().unwrap_or(0);
            let waiting = s["waiting"].as_array().map_or(0, Vec::len);
            let longest = s["waiting"]
                .as_array()
                .map(|w| {
                    w.iter()
                        .map(|x| x["wait_secs"].as_f64().unwrap_or(0.0))
                        .fold(0.0, f64::max)
                })
                .unwrap_or(0.0);
            format!(
                "slots {}/{} build {}/{} suite ({} waiting, longest {})",
                held("build"),
                cap("build"),
                held("suite"),
                cap("suite"),
                waiting,
                crate::slots::fmt_wait(longest)
            )
        }
        None => "slots: daemon unreachable".to_string(),
    };
    check(
        name,
        level,
        json!({
            "load1": load1, "cpus": cpus, "load_ratio": ratio,
            "io_stall_avg10": io_stall,
            "slots": scan.slots.as_ref().map(|s| s["waiting"]
                .as_array().map_or(0, Vec::len)),
        }),
        threshold,
        format!(
            "load1 {} ({}x of {} cpus), io stall {}, {}",
            load1
                .map(|l| format!("{l:.1}"))
                .unwrap_or_else(|| "-".into()),
            ratio
                .map(|r| format!("{r:.1}"))
                .unwrap_or_else(|| "-".into()),
            cpus as u64,
            io_stall
                .map(|s| format!("{s:.0}% avg10"))
                .unwrap_or_else(|| "-".into()),
            slots_text
        ),
        if level == Level::Ok {
            String::new()
        } else {
            "cadence build-slot status  # who holds the build slots".to_string()
        },
    )
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
            cargo_target_dir: None,
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
            slots: None,
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

    /// proc/<pid>/ shaped for the session census: caller controls
    /// comm, ppid, cwd and the metric files (`status` VmSwap,
    /// `smaps_rollup` Pss) — the census never reads argv.
    fn add_session_proc(
        proc: &Path,
        pid: u32,
        ppid: u32,
        comm: &str,
        cwd: Option<&Path>,
        age_secs: u64,
        metrics: (Option<u64>, Option<u64>), // (smaps_rollup Pss kB, status VmSwap kB)
    ) -> PathBuf {
        let dir = proc.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        let starttime = (1_000_000_u64.saturating_sub(age_secs)) * hz;
        // Fields after `)`: state, ppid, 17 fillers to field 21,
        // then starttime (22) and two trailing fields.
        std::fs::write(
            dir.join("stat"),
            format!(
                "{pid} ({comm}) S {ppid} {} {starttime} 0 0",
                "1 ".repeat(17)
            ),
        )
        .unwrap();
        std::fs::write(proc.join("uptime"), "1000000.00 0.00\n").unwrap();
        if let Some(cwd) = cwd {
            std::os::unix::fs::symlink(cwd, dir.join("cwd")).unwrap();
        }
        if let Some(kb) = metrics.1 {
            // The real uid line — a test overwriting this file is how
            // a foreign-user process is faked.
            let euid = unsafe { libc::geteuid() };
            std::fs::write(
                dir.join("status"),
                format!(
                    "Name:\t{comm}\nPid:\t{pid}\nUid:\t{euid}\t{euid}\t{euid}\t{euid}\nVmSwap:\t{kb} kB\n"
                ),
            )
            .unwrap();
        }
        if let Some(kb) = metrics.0 {
            std::fs::write(
                dir.join("smaps_rollup"),
                format!("{pid}\nPss:               {kb} kB\nPss_Anon:          {kb} kB\n"),
            )
            .unwrap();
        }
        dir
    }

    /// A minimal `cadence.sqlite3` for the census's read-only open —
    /// the two tables it queries, no migrations needed.
    fn fake_registry(state_dir: &Path) -> rusqlite::Connection {
        std::fs::create_dir_all(state_dir).unwrap();
        let conn = rusqlite::Connection::open(state_dir.join("cadence.sqlite3")).unwrap();
        conn.execute_batch(
            "CREATE TABLE agents(
                alias TEXT PRIMARY KEY, provider TEXT NOT NULL,
                endpoint_kind TEXT NOT NULL, role TEXT NOT NULL,
                cwd TEXT NOT NULL, sandbox TEXT NOT NULL,
                instructions TEXT, thread_id TEXT, session_id TEXT,
                model TEXT, pid INTEGER, endpoint TEXT, params TEXT,
                generation TEXT, state TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1, error TEXT,
                created REAL NOT NULL, updated REAL NOT NULL);
             CREATE TABLE messages(
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT UNIQUE NOT NULL, alias TEXT NOT NULL,
                body TEXT NOT NULL, reply_to TEXT, source TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'queued',
                turn_id TEXT, result TEXT, error TEXT,
                created REAL NOT NULL, started REAL, completed REAL);",
        )
        .unwrap();
        conn
    }

    /// Wall-clock now — `agents.updated` is REAL seconds, and the
    /// join fences claims whose live pid postdates the row's last
    /// write, so fixtures need realistic values.
    fn now_epoch() -> f64 {
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }

    #[allow(clippy::too_many_arguments)]
    fn add_agent(
        conn: &rusqlite::Connection,
        alias: &str,
        kind: &str,
        pid: Option<u32>,
        generation: Option<&str>,
        state: &str,
        cwd: &Path,
        updated: f64,
    ) {
        conn.execute(
            "INSERT INTO agents(alias, provider, endpoint_kind, role, cwd, sandbox, \
             pid, generation, state, created, updated) \
             VALUES (?1, 'claude', ?2, 'dev', ?6, 'none', ?3, ?4, ?5, 1.0, ?7)",
            rusqlite::params![
                alias,
                kind,
                pid.map(|p| p as i64),
                generation,
                state,
                cwd.to_string_lossy().to_string(),
                updated
            ],
        )
        .unwrap();
    }

    fn add_message(
        conn: &rusqlite::Connection,
        alias: &str,
        state: &str,
        turn_id: Option<&str>,
        completed: Option<f64>,
    ) {
        conn.execute(
            "INSERT INTO messages(id, alias, body, source, state, turn_id, created, completed) \
             VALUES (lower(hex(randomblob(8))), ?1, 'b', 't', ?2, ?3, 1.0, ?4)",
            rusqlite::params![alias, state, turn_id, completed],
        )
        .unwrap();
    }

    /// `<pm>/<key>/project.yaml` — one registered repo path.
    fn add_project(pm: &Path, key: &str, repo_path: &Path) {
        let dir = pm.join(key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.yaml"),
            format!(
                "key: {key}\nprefix: {key}-\nrepos:\n  - path: {}\n",
                repo_path.display()
            ),
        )
        .unwrap();
    }

    /// The `sessions` check's value object out of a full `run`.
    fn sessions_value(scan: &Scan) -> Value {
        let report = run(scan);
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "sessions")
            .unwrap()["value"]
            .clone()
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

    /// Puts `mode` back before the owning `TempDir` is removed. Declare
    /// it after the `TempDir` so this drops first.
    struct RestoreMode {
        path: PathBuf,
        mode: u32,
    }

    impl Drop for RestoreMode {
        fn drop(&mut self) {
            let _ =
                std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
        }
    }

    fn deny_directory(path: &Path) -> RestoreMode {
        let restore = RestoreMode {
            path: path.to_path_buf(),
            mode: 0o755,
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        restore
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

    /// CAD-141. Before the blob split, each of these scripts is one
    /// argv element and `redact_argv` returned it unchanged — the
    /// secret was still in the display string. Assertions check that
    /// the secret is gone and that benign context remains; they do
    /// not pin an incidental spelling of the mask.
    #[test]
    fn redact_argv_shell_blob_hides_nested_secrets() {
        let hidden_kept: &[(&str, &[&str], &[&str])] = &[
            (
                "run --token s3cr3tvalue && echo ok",
                &["s3cr3tvalue"],
                &["run", "--token", "echo ok"],
            ),
            (
                "run --token=s3cr3tvalue --verbose",
                &["s3cr3tvalue"],
                &["run", "--token", "--verbose"],
            ),
            (
                "run --token=\"s3cr3tvalue\" --verbose",
                &["s3cr3tvalue"],
                &["run", "--verbose"],
            ),
            (
                "run --token='s3cr3tvalue' --verbose",
                &["s3cr3tvalue"],
                &["run", "--verbose"],
            ),
            (
                "echo \"run --token s3cr3tvalue\"",
                &["s3cr3tvalue"],
                &["echo"],
            ),
            (
                "export TOKEN=\"s3cr3tvalue\" && echo ok",
                &["s3cr3tvalue"],
                &["export", "echo ok"],
            ),
            (
                "env GITHUB_TOKEN=ghp_TESTTOKEN cmd",
                &["ghp_TESTTOKEN"],
                &["env", "cmd"],
            ),
            (
                "curl -H 'Authorization: Basic YWxpY2U6c3VwZXJzZWNyZXQ=' https://api.x",
                &["YWxpY2U6c3VwZXJzZWNyZXQ"],
                &["curl", "https://api.x"],
            ),
            (
                "curl -H \"X-Custom: sk-LIVE\" https://api.x",
                &["sk-LIVE"],
                &["curl", "https://api.x"],
            ),
            (
                "psql postgres://admin:hunter2@db.example.com:5432/app",
                &["hunter2"],
                &["psql", "postgres://admin:", "db.example.com"],
            ),
            ("echo figd_TESTTOKEN", &["figd_TESTTOKEN"], &["echo"]),
            ("mysql -phunter2 db", &["hunter2"], &["mysql", "db"]),
            (
                "echo \"say 'run --token s3cr3tvalue'\"",
                &["s3cr3tvalue"],
                &["echo"],
            ),
            (
                "export MSG=\"run --token s3cr3tvalue\"",
                &["s3cr3tvalue"],
                &["export"],
            ),
            (
                "EDITOR=vim cmd --token s3cr3tvalue",
                &["s3cr3tvalue"],
                &["EDITOR=vim", "cmd"],
            ),
            (
                "curl -H Authorization: Basic s3cr3tvalue https://api.x",
                &["s3cr3tvalue"],
                &["curl"],
            ),
            (
                "tool --password=\"hello hunter2\" --verbose",
                &["hunter2"],
                &["tool", "--verbose"],
            ),
        ];
        for (script, hidden, kept) in hidden_kept {
            let out = redact_argv(&["sh", "-c", script]);
            for secret in *hidden {
                assert!(
                    !out.contains(secret),
                    "leaked {secret} from {script:?} -> {out}"
                );
            }
            for bit in *kept {
                assert!(out.contains(bit), "lost {bit} from {script:?} -> {out}");
            }
            assert!(
                out.contains("[REDACTED]"),
                "no mask from {script:?} -> {out}"
            );
        }
        // A secret-named assignment that fills the element still
        // hides the value; the trailing word is withheld with it.
        let sealed = redact_argv(&["sh", "-c", "PGPASSWORD=hunter2 psql"]);
        assert!(!sealed.contains("hunter2"), "{sealed}");
        assert!(sealed.contains("[REDACTED]"), "{sealed}");
        // Same secret behind `export` keeps the following command.
        let exported = redact_argv(&["sh", "-c", "export PGPASSWORD=hunter2 psql"]);
        assert!(!exported.contains("hunter2"), "{exported}");
        assert!(exported.contains("psql"), "{exported}");
        // Direct `--password=two words` must not split the tail back out.
        let direct = redact_argv(&["tool", "--password=hello hunter2"]);
        assert_eq!(direct, "tool --password=[REDACTED]");
        assert!(!direct.contains("hunter2"));
    }

    /// Benign command text, including quotes, `$`, ports, SHAs and
    /// ordinary flags, stays byte-identical. Malformed text is
    /// withheld only when a secret is still visible.
    #[test]
    fn redact_argv_shell_blob_keeps_benign_text() {
        for script in [
            "echo hello && ls /tmp",
            "echo 'hello world'",
            "echo \"hello world\"",
            "export MSG=\"hello world\" && echo ok",
            "echo don't stop",
            "git checkout 4f2a9c1d8e3b5a7c9f1e2d3b4a5c6d7e8f9a0b1c",
            "npm publish --access public",
            "ssh -p 2222 host",
            "t monkey=banana",
            "echo $HOME && ls /tmp",
            "echo \"hello",
        ] {
            assert_eq!(
                redact_argv(&["sh", "-c", script]),
                format!("sh -c {script}"),
                "{script}"
            );
        }
        // Unbalanced quote, or `$`, next to a real secret: withhold
        // rather than print the value. No literal secret survives.
        for script in [
            "run --token \"s3cr3tvalue",
            "run --token s3cr3tvalue && echo $HOME",
        ] {
            let out = redact_argv(&["sh", "-c", script]);
            assert!(!out.contains("s3cr3tvalue"), "{script} -> {out}");
            assert!(out.contains("[REDACTED]"), "{script} -> {out}");
        }
    }

    /// Unicode whitespace is not a shell separator. The QA fixture
    /// `run --token<NBSP>s3cr3tvalue` is still ambiguous
    /// credential-bearing diagnostic text: the value must not remain
    /// visible, while benign Unicode text is unchanged.
    #[test]
    fn redact_argv_unicode_whitespace_hides_flag_value() {
        let secret = "s3cr3tvalue";
        let ascii = redact_argv(&["sh", "-c", "run --token s3cr3tvalue && echo ok"]);
        assert!(!ascii.contains(secret), "{ascii}");
        assert!(ascii.contains("echo ok"), "{ascii}");
        assert!(ascii.contains("--token"), "{ascii}");
        // NBSP, then em space — one other Unicode space.
        for sep in ['\u{00a0}', '\u{2003}'] {
            let script = format!("run --token{sep}{secret}");
            let out = redact_argv(&["sh", "-c", &script]);
            assert!(
                !out.contains(secret),
                "U+{:04X} leaked in {out}",
                sep as u32
            );
            assert!(out.contains("[REDACTED]"), "U+{:04X} {out}", sep as u32);
            assert!(out.contains("run"), "{out}");
            assert!(out.contains("--token"), "{out}");
            let glued = format!("--token{sep}{secret}");
            let direct = redact_argv(&["tool", &glued]);
            assert!(!direct.contains(secret), "U+{:04X} {direct}", sep as u32);
            assert!(direct.contains("--token"), "{direct}");
        }
        for script in [
            "echo café",
            "echo hello\u{00a0}world",
            "echo hello\u{2003}world",
        ] {
            assert_eq!(
                redact_argv(&["sh", "-c", script]),
                format!("sh -c {script}"),
                "{script:?}"
            );
        }
    }

    #[test]
    fn orphans_redact_shell_blob_argv() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = scan.cwd.clone();
        add_pid_argv(
            &scan.proc_root,
            43,
            Some(&repo.join(".cadence/wt/gone")),
            None,
            Some(&["sh", "-c", "run --token s3cr3tvalue && echo ok"]),
            7_200,
            &[],
        );
        let c = check_orphans(&scan);
        let blob = serde_json::to_string(&c.to_json()).unwrap();
        for text in [&blob, &c.detail, &c.remedy] {
            assert!(!text.contains("s3cr3tvalue"), "{text}");
        }
        assert!(blob.contains("[REDACTED]"), "{blob}");
        assert!(blob.contains("echo ok"), "{blob}");
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

    // ---------- legacy task cargo targets ----------

    fn task_row<'a>(value: &'a Value, name: &str) -> &'a Value {
        value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("no task-target row {name} in {value}"))
    }

    #[test]
    fn legacy_task_target_name_matches_only_the_missed_shape() {
        for name in [
            "cad156-fix-target",
            "cad173-nextest-target-one",
            "cad176-pr100-target",
            "cad9-target",
        ] {
            assert!(
                legacy_task_target_name(std::ffi::OsStr::new(name)),
                "{name}"
            );
        }
        for name in [
            "cadence-issue-at-1-2",
            "cadence-smoke",
            ".tmpAbc",
            "tmp.XYZ",
            "unrelated",
            "my-target",
            "cad-target",
            "cad156-fix",
            "CAD156-fix-target",
            "notcad156-fix-target",
            "target",
        ] {
            assert!(
                !legacy_task_target_name(std::ffi::OsStr::new(name)),
                "{name} must stay outside the inventory"
            );
        }
    }

    #[test]
    fn task_targets_list_missed_names_and_skip_unrelated() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let tmp = scan.temp_dir.clone();
        for name in [
            "cad156-fix-target",
            "cad173-nextest-target-one",
            "cad176-pr100-target",
        ] {
            let dir = tmp.join(name);
            std::fs::create_dir_all(dir.join("debug")).unwrap();
            real_bytes(&dir.join("debug/lib.rlib"), 4096);
            set_mtime_old(&dir, 90_000);
        }
        // Same age as a leak the temp-dirs check would delete — these
        // names must not join that `rm -rf` list.
        for name in [
            "cad156-fix-target",
            "cad173-nextest-target-one",
            "cad176-pr100-target",
        ] {
            set_mtime_old(&tmp.join(name), 90_000);
        }
        for name in [
            "unrelated",
            "cadence-issue-at-1-2",
            "cadence-smoke",
            "my-target",
            "cad-target",
            "cad156-fix",
            "target",
        ] {
            std::fs::create_dir_all(tmp.join(name)).unwrap();
            set_mtime_old(&tmp.join(name), 90_000);
        }
        std::fs::write(tmp.join("cad156-fix-target-file"), b"not a dir").unwrap();

        let temps = check_temp_dirs(&scan);
        assert_eq!(temps.value["count"].as_u64(), Some(2));
        let listed: Vec<&str> = temps.value["dirs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d["path"].as_str())
            .collect();
        assert!(listed
            .iter()
            .all(|p| !p.contains("cad156") && !p.contains("cad173") && !p.contains("cad176")));

        let c = check_task_targets(&scan);
        assert_eq!(c.level, Level::Ok, "{}", c.detail);
        assert_eq!(c.value["count"], 3);
        assert_eq!(c.value["safe_to_delete"], false);
        assert_eq!(c.value["record_search"], "complete");
        assert!(!c.remedy.contains("rm"));
        assert!(c.remedy.contains("read-only"));
        assert!(c.detail.contains("not proof"));
        let names: Vec<&str> = c.value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "cad156-fix-target",
                "cad173-nextest-target-one",
                "cad176-pr100-target",
            ]
        );
        for name in &names {
            let row = task_row(&c.value, name);
            assert_eq!(row["ownership"], "name-only");
            assert_eq!(row["proven"], false);
            assert_eq!(row["activity"], "unproven");
            assert_eq!(row["cwd_exe"], "none-observed");
            assert_eq!(row["cargo_lock"], "absent");
            assert_eq!(row["safe_to_delete"], false);
            assert_eq!(row["reclaim_candidate"], false);
            assert_eq!(row["action"], "none");
            assert_eq!(row["followed"], false);
            assert_eq!(row["exclude"], "name-only");
            assert!(row["age_secs"].as_u64().unwrap() >= 80_000);
            assert!(row["bytes"].as_u64().unwrap() > 0);
            assert_eq!(row["bytes_truncated"], false);
        }
    }

    #[test]
    fn task_targets_do_not_follow_foreign_symlinks() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let tmp = scan.temp_dir.clone();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        real_bytes(&outside.join("payload"), 50_000);
        std::os::unix::fs::symlink(&outside, tmp.join("cad999-foreign-target")).unwrap();
        // A relative link that escapes the temp dir is foreign too.
        std::os::unix::fs::symlink("../outside", tmp.join("cad998-rel-target")).unwrap();
        // A link that stays inside the temp dir is still not walked.
        let inside = tmp.join("cad997-inside-target");
        std::fs::create_dir_all(&inside).unwrap();
        real_bytes(&inside.join("payload"), 50_000);
        std::os::unix::fs::symlink(&inside, tmp.join("cad997-link-target")).unwrap();

        let c = check_task_targets(&scan);
        assert_eq!(c.level, Level::Warn, "{}", c.detail);
        for name in [
            "cad999-foreign-target",
            "cad998-rel-target",
            "cad997-link-target",
        ] {
            let row = task_row(&c.value, name);
            assert_eq!(row["symlink"], true);
            assert_eq!(row["ancestor_symlink"], false);
            assert_eq!(row["followed"], false);
            assert_eq!(row["bytes"], Value::Null);
            assert_eq!(row["bytes_skipped"], "symlink");
            assert_eq!(row["cargo_lock"], "unknown");
            assert_eq!(row["safe_to_delete"], false);
            assert_eq!(row["action"], "none");
            assert_eq!(row["exclude"], "symlink");
            let blob = serde_json::to_string(row).unwrap();
            assert!(!blob.contains("payload"), "{blob}");
        }
        assert_eq!(
            task_row(&c.value, "cad999-foreign-target")["foreign_symlink"],
            true
        );
        assert_eq!(
            task_row(&c.value, "cad998-rel-target")["foreign_symlink"],
            true
        );
        assert_eq!(
            task_row(&c.value, "cad997-link-target")["foreign_symlink"],
            false
        );
        // The real directory the inside link points at is its own row
        // and is measured; the link row must not have added those bytes.
        let inside_row = task_row(&c.value, "cad997-inside-target");
        assert!(inside_row["bytes"].as_u64().unwrap() > 0);
        assert_eq!(
            task_row(&c.value, "cad997-link-target")["bytes"],
            Value::Null
        );
    }

    fn mkfifo(path: &Path) {
        let c = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o644) },
            0,
            "mkfifo {}",
            path.display()
        );
    }

    /// The probe must return. A regression that blocks in `open` fails
    /// this instead of hanging the suite.
    fn probe_lock_bounded(path: &Path) -> LockBit {
        let path = path.to_path_buf();
        let shown = path.display().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(probe_lock_file(&path));
        });
        rx.recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|_| panic!("probe_lock_file blocked on {shown}"))
    }

    fn check_targets_bounded(scan: Scan) -> Check {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(check_task_targets(&scan));
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("task-target inventory blocked")
    }

    #[test]
    fn task_targets_lock_probe_rejects_fifo_and_symlink_without_blocking() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let fifo_dir = scan.temp_dir.join("cad410-fifo-target");
        std::fs::create_dir_all(fifo_dir.join("debug")).unwrap();
        mkfifo(&fifo_dir.join(".cargo-lock"));
        mkfifo(&fifo_dir.join("debug/.cargo-lock"));

        let link_dir = scan.temp_dir.join("cad411-locklink-target");
        std::fs::create_dir_all(&link_dir).unwrap();
        let fifo = root.path().join("elsewhere-fifo");
        mkfifo(&fifo);
        std::os::unix::fs::symlink(&fifo, link_dir.join(".cargo-lock")).unwrap();

        let free_dir = scan.temp_dir.join("cad412-freelock-target");
        std::fs::create_dir_all(&free_dir).unwrap();
        std::fs::write(free_dir.join(".cargo-lock"), b"lock").unwrap();

        assert_eq!(
            probe_lock_bounded(&fifo_dir.join(".cargo-lock")),
            LockBit::Unknown
        );
        assert_eq!(
            probe_lock_bounded(&fifo_dir.join("debug/.cargo-lock")),
            LockBit::Unknown
        );
        assert_eq!(
            probe_lock_bounded(&link_dir.join(".cargo-lock")),
            LockBit::Unknown
        );
        assert_eq!(
            probe_lock_bounded(&free_dir.join(".cargo-lock")),
            LockBit::Free
        );

        let c = check_targets_bounded(scan);
        for name in ["cad410-fifo-target", "cad411-locklink-target"] {
            let row = task_row(&c.value, name);
            assert_eq!(row["cargo_lock"], "unknown", "{name}");
            assert_eq!(row["followed"], false, "{name}");
            assert_eq!(row["safe_to_delete"], false, "{name}");
            assert_eq!(row["action"], "none", "{name}");
        }
        assert_eq!(
            task_row(&c.value, "cad412-freelock-target")["cargo_lock"],
            "free"
        );
        assert!(!c.remedy.contains("rm"));
    }

    #[test]
    fn task_targets_do_not_follow_ancestor_symlinks() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        let real = root.path().join("real-cache");
        let hidden = real.join("cad400-ancestor-target");
        std::fs::create_dir_all(&hidden).unwrap();
        real_bytes(&hidden.join("payload.bin"), 50_000);
        mkfifo(&hidden.join(".cargo-lock"));
        let via = scan.temp_dir.join("via-link");
        std::os::unix::fs::symlink(&real, &via).unwrap();
        let through = via.join("cad400-ancestor-target");
        // Final-component lstat would follow `via-link` and then block
        // on the FIFO. The probe must stop at the ancestor link.
        assert_eq!(
            probe_lock_bounded(&through.join(".cargo-lock")),
            LockBit::Unknown
        );
        assert!(matches!(
            lexical_kind(&through),
            LexicalKind::Symlink {
                final_component: false,
                ..
            }
        ));
        scan.cargo_target_dir = Some(through);
        let c = check_targets_bounded(scan);
        let row = task_row(&c.value, "cad400-ancestor-target");
        assert_eq!(row["ancestor_symlink"], true);
        assert_eq!(row["symlink"], true);
        assert_eq!(row["followed"], false);
        assert_eq!(row["bytes"], Value::Null);
        assert_eq!(row["bytes_skipped"], "symlink");
        assert_eq!(row["cargo_lock"], "unknown");
        assert_eq!(row["foreign_symlink"], true);
        assert_eq!(row["safe_to_delete"], false);
        assert_eq!(row["reclaim_candidate"], false);
        assert_eq!(row["action"], "none");
        let blob = serde_json::to_string(&c.to_json()).unwrap();
        assert!(!blob.contains("payload.bin"), "{blob}");
        assert!(!blob.contains("rm "), "{blob}");
    }

    #[test]
    fn task_targets_skip_foreign_uid_walks() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        scan.uid = scan.uid.wrapping_add(1);
        let dir = scan.temp_dir.join("cad156-fix-target");
        std::fs::create_dir_all(dir.join("debug")).unwrap();
        real_bytes(&dir.join("debug/lib.rlib"), 20_000);
        // A symlink planted inside must not be followed just because
        // the name matched: the whole tree is someone else's.
        std::os::unix::fs::symlink("/etc", dir.join("debug/escape")).unwrap();

        let c = check_task_targets(&scan);
        let row = task_row(&c.value, "cad156-fix-target");
        assert_eq!(row["uid_matches"], false);
        assert_eq!(row["bytes"], Value::Null);
        assert_eq!(row["bytes_skipped"], "foreign-uid");
        assert_eq!(row["cargo_lock"], "unknown");
        assert_eq!(row["activity"], "unknown");
        assert_eq!(row["exclude"], "foreign-uid");
        assert_eq!(row["safe_to_delete"], false);
        assert_eq!(row["followed"], false);
        assert_eq!(c.level, Level::Warn);
        let blob = serde_json::to_string(&c.to_json()).unwrap();
        assert!(!blob.contains("escape"), "{blob}");
    }

    #[test]
    fn task_targets_live_lock_and_cwd_are_active_not_safe() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let live = scan.temp_dir.join("cad176-pr100-target");
        let idle = scan.temp_dir.join("cad156-fix-target");
        let decoy = scan.temp_dir.join("cad156-fix-target-extra");
        for dir in [&live, &idle, &decoy] {
            std::fs::create_dir_all(dir.join("debug")).unwrap();
        }
        let lock_path = live.join("debug/.cargo-lock");
        std::fs::write(&lock_path, b"lock").unwrap();
        let _held = crate::worktree::TestFileLock::acquire(&lock_path);
        add_pid(
            &scan.proc_root,
            176,
            Some(&live.join("debug")),
            None,
            Some("cargo test --lib host_inventory_marker"),
            30,
            &[],
        );
        // Component-wise: this cwd is the sibling, not the live dir.
        add_pid(
            &scan.proc_root,
            177,
            Some(&decoy),
            Some(&live.join("debug/cadence")),
            None,
            30,
            &[],
        );

        let c = check_task_targets(&scan);
        assert_eq!(c.level, Level::Warn, "{}", c.detail);
        let live_row = task_row(&c.value, "cad176-pr100-target");
        assert_eq!(live_row["cargo_lock"], "held");
        assert_eq!(live_row["activity"], "active");
        assert_eq!(live_row["cwd_exe"], "observed");
        assert_eq!(live_row["exclude"], "active");
        assert_eq!(live_row["safe_to_delete"], false);
        assert_eq!(live_row["reclaim_candidate"], false);
        assert_eq!(live_row["action"], "none");
        let pids: Vec<u64> = live_row["pids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_u64().unwrap())
            .collect();
        assert_eq!(pids, vec![176, 177]);
        let idle_row = task_row(&c.value, "cad156-fix-target");
        assert_eq!(idle_row["activity"], "unproven");
        assert_eq!(idle_row["cwd_exe"], "none-observed");
        assert_eq!(idle_row["cargo_lock"], "absent");
        assert_eq!(idle_row["safe_to_delete"], false);
        assert!(idle_row["pids"].as_array().unwrap().is_empty());
        let decoy_row = task_row(&c.value, "cad156-fix-target-extra");
        let decoy_pids: Vec<u64> = decoy_row["pids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_u64().unwrap())
            .collect();
        assert_eq!(decoy_pids, vec![177]);
        assert!(!decoy_row["pids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_u64() == Some(176)));
        let blob = serde_json::to_string(&c.to_json()).unwrap();
        // The synthetic cmdline is a neutral marker. This inventory
        // must not echo process arguments.
        assert!(!blob.contains("host_inventory_marker"), "{blob}");
        assert!(!blob.contains("rm -rf"), "{blob}");
        assert!(c.detail.contains("not proof"));
    }

    #[test]
    fn task_targets_recorded_and_configured_are_proven() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        let recorded = scan.temp_dir.join("recorded-custom-out");
        let named = scan.temp_dir.join("cad200-only-target");
        let both = scan.temp_dir.join("cad201-recorded-target");
        let configured = scan.cwd.join(".cadence/target/shared");
        for dir in [&recorded, &named, &both, &configured] {
            std::fs::create_dir_all(dir).unwrap();
        }
        real_bytes(&named.join("lib.rlib"), 128);
        scan.cargo_target_dir = Some(PathBuf::from(".cadence/target/shared"));
        let pm = scan.pm_dir.clone().unwrap();
        write_issue(
            &pm,
            "cadence",
            "CAD-201",
            "doing",
            &format!(
                "refs:\n  - kind: worktree\n    path: {}\n    cargo_target: {}\n  - kind: worktree\n    path: {}\n    cargo_target: {}\n",
                scan.cwd.join(".cadence/wt/cad-201").display(),
                both.display(),
                scan.cwd.join(".cadence/wt/custom").display(),
                recorded.display(),
            ),
        );

        let c = check_task_targets(&scan);
        assert_eq!(c.value["record_search"], "complete");
        assert_eq!(c.value["record_conclusive"], true);
        let named_row = task_row(&c.value, "cad200-only-target");
        assert_eq!(named_row["ownership"], "name-only");
        assert_eq!(named_row["proven"], false);
        assert_eq!(named_row["safe_to_delete"], false);
        let both_row = task_row(&c.value, "cad201-recorded-target");
        assert_eq!(both_row["ownership"], "recorded");
        assert_eq!(both_row["proven"], true);
        assert_eq!(both_row["name_match"], true);
        assert_eq!(both_row["issues"][0], "CAD-201");
        assert_eq!(both_row["safe_to_delete"], false);
        assert_eq!(both_row["reclaim_candidate"], false);
        let recorded_row = task_row(&c.value, "recorded-custom-out");
        assert_eq!(recorded_row["ownership"], "recorded");
        assert_eq!(recorded_row["proven"], true);
        assert_eq!(recorded_row["name_match"], false);
        assert_eq!(recorded_row["pressure"], true);
        let configured_row = task_row(&c.value, "shared");
        assert_eq!(configured_row["ownership"], "configured");
        assert_eq!(configured_row["proven"], true);
        assert_eq!(configured_row["configured"], true);
        assert_eq!(configured_row["pressure"], false);
        assert_eq!(configured_row["bytes_skipped"], "tracked-elsewhere");
        assert_eq!(configured_row["safe_to_delete"], false);
        // Unrelated names stay out even when a tracker is present.
        assert!(c.value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["name"] != "unrelated"));
        let blob = serde_json::to_string(&c.to_json()).unwrap();
        assert!(!blob.contains("rm "), "{blob}");
    }

    #[test]
    fn task_targets_unreadable_tracker_does_not_prove_name_only() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let pm = scan.pm_dir.clone().unwrap();
        std::fs::remove_dir_all(&pm).unwrap();
        std::fs::write(&pm, b"not a directory").unwrap();
        std::fs::create_dir_all(scan.temp_dir.join("cad156-fix-target")).unwrap();
        let c = check_task_targets(&scan);
        assert_eq!(c.value["record_search"], "unreadable");
        assert_eq!(c.value["record_conclusive"], false);
        let row = task_row(&c.value, "cad156-fix-target");
        assert_eq!(row["ownership"], "name-only");
        assert_eq!(row["proven"], false);
        assert_eq!(row["safe_to_delete"], false);
    }

    #[test]
    fn dir_size_limited_unreadable_directory_is_truncated() {
        let root = TempDir::new().unwrap();
        let missing = root.path().join("missing-cad233");
        let (bytes, truncated, visited) = dir_size_limited(&missing, 32);
        assert_eq!(bytes, 0);
        assert!(!truncated);
        assert_eq!(visited, 0);

        let dir = root.path().join("sized");
        std::fs::create_dir_all(dir.join("open")).unwrap();
        real_bytes(&dir.join("open/seen.bin"), 4096);
        let (open_bytes, open_truncated, _) = dir_size_limited(&dir, 32);
        assert!(!open_truncated);
        assert!(open_bytes >= 4096);

        let secret = dir.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        real_bytes(&secret.join("hidden.bin"), 80_000);
        let _restore = deny_directory(&secret);
        let (bytes, truncated, _) = dir_size_limited(&dir, 32);
        assert!(truncated);
        assert!(bytes < 80_000, "hidden bytes were counted: {bytes}");
        assert!(bytes >= open_bytes);
    }

    #[test]
    fn task_targets_unreadable_child_is_not_a_finished_measurement() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let dir = scan.temp_dir.join("cad420-partial-target");
        std::fs::create_dir_all(dir.join("secret")).unwrap();
        real_bytes(&dir.join("seen.bin"), 2048);
        real_bytes(&dir.join("secret/hidden.bin"), 80_000);
        let _restore = deny_directory(&dir.join("secret"));
        let c = check_task_targets(&scan);
        let row = task_row(&c.value, "cad420-partial-target");
        assert_eq!(row["bytes_truncated"], true);
        assert_eq!(row["safe_to_delete"], false);
        assert_eq!(row["reclaim_candidate"], false);
        assert_eq!(row["action"], "none");
        assert_eq!(c.value["scan_truncated"], true);
        assert_eq!(c.value["safe_to_delete"], false);
        assert_eq!(c.level, Level::Warn);
        assert!(row["bytes"].as_u64().unwrap() < 80_000);
        let blob = serde_json::to_string(&c.to_json()).unwrap();
        assert!(!blob.contains("rm "), "{blob}");
    }

    #[test]
    fn task_targets_denied_issue_record_is_not_conclusive() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let pm = scan.pm_dir.clone().unwrap();
        let hidden = pm.join("cadence").join("CAD-420");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(
            hidden.join("issue.md"),
            format!(
                "---\nid: CAD-420\ntitle: t\nstatus: doing\npriority: P2\nrefs:\n  - kind: worktree\n    path: {}\n    cargo_target: {}\ncreated: 2026-09-19T00:00:00Z\n---\n\nbody\n",
                scan.cwd.display(),
                scan.temp_dir.join("cad420-recorded-target").display(),
            ),
        )
        .unwrap();
        write_issue(&pm, "cadence", "CAD-421", "doing", "");
        let _restore = deny_directory(&hidden);
        std::fs::create_dir_all(scan.temp_dir.join("cad420-recorded-target")).unwrap();
        let c = check_task_targets(&scan);
        assert_eq!(c.value["record_search"], "incomplete");
        assert_eq!(c.value["record_conclusive"], false);
        let row = task_row(&c.value, "cad420-recorded-target");
        assert_eq!(row["ownership"], "name-only");
        assert_eq!(row["proven"], false);
        assert_eq!(row["safe_to_delete"], false);
        assert_eq!(row["reclaim_candidate"], false);
        assert_eq!(c.level, Level::Warn);
        assert_ne!(c.detail, "none");
    }

    #[test]
    fn task_targets_unreadable_tracker_with_no_rows_is_not_none() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let pm = scan.pm_dir.clone().unwrap();
        std::fs::remove_dir_all(&pm).unwrap();
        std::fs::write(&pm, b"not a directory").unwrap();
        let c = check_task_targets(&scan);
        assert_eq!(c.value["record_search"], "unreadable");
        assert_eq!(c.value["record_conclusive"], false);
        assert_eq!(c.value["count"], 0);
        assert_eq!(c.value["safe_to_delete"], false);
        assert_ne!(c.detail, "none");
        assert!(c.detail.contains("unreadable"), "{}", c.detail);
        assert_eq!(c.level, Level::Warn);
        assert!(c.remedy.is_empty());
    }

    #[test]
    fn task_targets_denied_proc_dir_is_not_a_complete_idle_scan() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let pid = scan.proc_root.join("4242");
        std::fs::create_dir_all(&pid).unwrap();
        let _restore = deny_directory(&pid);
        std::fs::create_dir_all(scan.temp_dir.join("cad421-proc-target")).unwrap();
        let c = check_task_targets(&scan);
        assert_eq!(c.value["proc_scan"], "partial");
        let row = task_row(&c.value, "cad421-proc-target");
        assert_eq!(row["cwd_exe"], "unreadable");
        assert_eq!(row["activity"], "unknown");
        assert_eq!(row["exclude"], "unknown");
        assert_eq!(row["safe_to_delete"], false);
        assert_eq!(row["reclaim_candidate"], false);
        assert_eq!(row["action"], "none");
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("not proof"), "{}", c.detail);
        assert!(c.detail.contains("partial"), "{}", c.detail);
        let body = c.to_json();
        assert_eq!(body["level"], "warn");
        assert_eq!(exit_code(&json!({"level": body["level"]})), 1);
    }

    #[test]
    fn task_targets_unreadable_proc_with_no_rows_is_not_a_healthy_gate() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let _restore = deny_directory(&scan.proc_root);
        let c = check_task_targets(&scan);
        assert_eq!(c.value["proc_scan"], "unreadable");
        assert_eq!(c.value["count"], 0);
        assert_eq!(c.value["safe_to_delete"], false);
        assert_eq!(c.level, Level::Warn);
        assert_ne!(c.detail, "none");
        assert!(c.detail.contains("unreadable"), "{}", c.detail);
        let body = c.to_json();
        assert_eq!(exit_code(&json!({"level": body["level"]})), 1);
        assert!(c.remedy.is_empty());
    }

    #[test]
    fn task_targets_unreadable_temp_dir_is_not_an_empty_inventory() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        std::fs::create_dir_all(scan.temp_dir.join("cad422-hidden-target")).unwrap();
        let _restore = deny_directory(&scan.temp_dir);
        let c = check_task_targets(&scan);
        assert_eq!(c.value["temp_scan"], "unreadable");
        assert_eq!(c.value["count"], 0);
        assert_eq!(c.value["safe_to_delete"], false);
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("unreadable"), "{}", c.detail);
        assert_ne!(c.detail, "none");
        assert!(c.remedy.is_empty());
    }

    #[test]
    fn task_targets_truncate_a_long_name_scan() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        for i in 0..=TASK_TARGET_ROW_CAP {
            std::fs::create_dir_all(scan.temp_dir.join(format!("cad{i:04}-row-target"))).unwrap();
        }
        let c = check_task_targets(&scan);
        assert_eq!(c.value["count"], TASK_TARGET_ROW_CAP);
        assert_eq!(c.value["scan_truncated"], true);
        assert_eq!(c.level, Level::Warn);
        assert!(task_row(&c.value, "cad0000-row-target")["safe_to_delete"] == false);
        assert!(c.value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["name"] != format!("cad{TASK_TARGET_ROW_CAP:04}-row-target")));
        assert!(!c.remedy.contains("rm"));
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
                "sessions",
                "orphans",
                "temp-dirs",
                "task-targets",
                "worktrees",
                "load",
                "config"
            ]
        );
        for c in report["checks"].as_array().unwrap() {
            for k in ["level", "value", "threshold", "detail", "remedy"] {
                assert!(c.get(k).is_some(), "check missing {k}");
            }
        }
        assert_eq!(exit_code(&report), 0, "{}", render(&report));
        let task = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "task-targets")
            .unwrap();
        assert_eq!(task["level"], "ok");
        assert_eq!(task["detail"], "none");
        assert_eq!(task["value"]["count"], 0);
        assert_eq!(task["value"]["safe_to_delete"], false);
        assert_eq!(task["remedy"], "");
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
        assert!(host_thresholds(Some(&pm)).config_error.is_none());
    }

    #[test]
    fn pm_yaml_host_errors_fail_the_wal_watch_closed() {
        let root = TempDir::new().unwrap();
        let pm = root.path().join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        // The explicit opt-out is honoured.
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  wal_checkpoint: false\n",
        )
        .unwrap();
        let t = host_thresholds(Some(&pm));
        assert!(!t.wal_checkpoint && t.config_error.is_none());
        // A quoted bool or a misspelled key is refused, named, and turns
        // the writer off instead of silently reverting to defaults.
        for (body, needle) in [
            (
                "  wal_checkpoint: \"false\"\n  wal_max_bytes: 4096\n",
                "wal_checkpoint",
            ),
            ("  wal_max_byte: 4096\n", "wal_max_byte"),
        ] {
            std::fs::write(pm.join("pm.yaml"), format!("schema: 1\nhost:\n{body}")).unwrap();
            let t = host_thresholds(Some(&pm));
            let err = t.config_error.clone().unwrap_or_default();
            assert!(err.contains(needle), "{needle}: {err}");
            assert!(!t.wal_checkpoint, "{needle}: WAL watch must be off");
            assert_eq!(t.wal_max_bytes, GIB, "{needle}: defaults in force");
            let mut scan = fake_scan(&root);
            scan.thresholds = t;
            let c = check_config(&scan);
            assert_eq!(c.level, Level::Warn);
            assert!(c.detail.contains(needle), "{}", c.detail);
        }
    }

    // ---------- session census (CAD-198) ----------

    #[test]
    fn sessions_tree_counts_members_once_and_sums_pss_swap() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // devin root → npm wrapper → node server (the MCP stack).
        add_session_proc(
            &proc,
            100,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(100), Some(50)),
        );
        add_session_proc(
            &proc,
            101,
            100,
            "npm",
            Some(&repo),
            300_000,
            (Some(20), Some(200)),
        );
        add_session_proc(
            &proc,
            102,
            101,
            "node",
            Some(&repo),
            300_000,
            (Some(30), Some(300)),
        );
        // A second, unrelated claude tree with its own child.
        add_session_proc(
            &proc,
            200,
            1,
            "claude",
            Some(&repo),
            100_000,
            (Some(10), Some(5)),
        );
        add_session_proc(
            &proc,
            201,
            200,
            "node",
            Some(&repo),
            90_000,
            (Some(4), Some(1)),
        );
        // A process outside any session.
        add_session_proc(
            &proc,
            300,
            1,
            "postgres",
            Some(&repo),
            10_000,
            (Some(1), Some(0)),
        );
        let v = sessions_value(&scan);
        let trees = v["trees"].as_array().unwrap();
        assert_eq!(trees.len(), 2, "{v}");
        let devin = trees
            .iter()
            .find(|t| t["root"]["family"] == "devin")
            .unwrap();
        assert_eq!(devin["procs"], 3);
        assert_eq!(devin["pss_bytes"], 150 * 1024);
        assert_eq!(devin["swap_bytes"], 550 * 1024);
        assert_eq!(devin["pss_missing_pids"], 0);
        // The wrapper stack is inside the tree total — never a
        // separate family sum and never double-counted.
        let pids: Vec<u64> = trees
            .iter()
            .flat_map(|t| {
                t["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m["pid"].as_u64().unwrap())
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut dedup = pids.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(pids.len(), dedup.len(), "a pid appears in two trees");
        assert!(pids.contains(&101) && pids.contains(&102));
        // pid+start identity is carried, never pid alone.
        assert_eq!(devin["root"]["pid"], 100);
        assert!(devin["root"]["start_jiffies"].as_u64().unwrap() > 0);
    }

    #[test]
    fn sessions_owned_via_managed_root_and_pty_ancestor() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // Managed: agents.pid IS the provider root.
        add_session_proc(
            &proc,
            200,
            1,
            "claude",
            Some(&repo),
            5_000,
            (Some(10), Some(1)),
        );
        // Pty: agents.pid is the pane shell (bash) above the devin root.
        add_session_proc(
            &proc,
            50,
            1,
            "bash",
            Some(&repo),
            400_000,
            (Some(1), Some(0)),
        );
        add_session_proc(
            &proc,
            100,
            50,
            "devin",
            Some(&repo),
            400_000,
            (Some(9), Some(2)),
        );
        let conn = fake_registry(&scan.state_dir);
        add_agent(
            &conn,
            "qa-1",
            "managed",
            Some(200),
            Some("g1"),
            "idle",
            &repo,
            now_epoch(),
        );
        add_agent(
            &conn,
            "devin-d",
            "pty",
            Some(50),
            Some("g2"),
            "idle",
            &repo,
            now_epoch(),
        );
        drop(conn);
        let v = sessions_value(&scan);
        let trees = v["trees"].as_array().unwrap();
        let by_alias = |a: &str| {
            trees
                .iter()
                .find(|t| t["alias"] == a)
                .unwrap_or_else(|| panic!("no tree owned by {a}: {v}"))
                .clone()
        };
        let managed = by_alias("qa-1");
        assert_eq!(managed["agreement"], "agreed");
        assert_eq!(managed["endpoint_kind"], "managed");
        assert_eq!(managed["generation"], "g1");
        assert_eq!(managed["reclaim"]["candidate"], false);
        assert_eq!(managed["reclaim"]["protected"], "owned — registry row qa-1");
        let pty = by_alias("devin-d");
        assert_eq!(pty["agreement"], "agreed");
        assert_eq!(pty["endpoint_pid"], 50);
        assert_eq!(pty["root"]["pid"], 100);
    }

    #[test]
    fn sessions_unowned_in_scope_is_a_dry_run_candidate() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // No registry at all — provably zero rows → ProcessOnly stands.
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(100), Some(80)),
        );
        add_session_proc(
            &proc,
            701,
            700,
            "node",
            Some(&repo),
            300_000,
            (Some(20), Some(40)),
        );
        let v = sessions_value(&scan);
        assert_eq!(v["store"], "absent");
        let tree = &v["trees"][0];
        assert_eq!(tree["agreement"], "process-only");
        assert_eq!(tree["scope"], "project:cadence");
        let r = &tree["reclaim"];
        assert_eq!(r["candidate"], true);
        // 300_000s ≈ 83h → high confidence.
        assert_eq!(r["confidence"], "high");
        assert_eq!(r["pss_bytes"], 120 * 1024);
        assert_eq!(r["swap_bytes"], 120 * 1024);
        assert_eq!(v["totals"]["candidates"], 1);
        // Dry-run only — the check carries no executable action.
        let report = run(&scan);
        let c = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "sessions")
            .unwrap();
        // Candidates are routine (a human ran an agent in a repo) and
        // stay ok — only evidence failure (unreadable store) warns.
        // The authorisation caveat travels in `detail`, since render()
        // never prints a remedy for an ok check.
        assert_eq!(c["level"], "ok");
        assert!(c["detail"]
            .as_str()
            .unwrap()
            .contains("separately authorised phase"));
        assert!(!c["remedy"].as_str().unwrap().contains("kill"));
        assert!(c["detail"].as_str().unwrap().contains("700("));
    }

    #[test]
    fn sessions_foreign_and_unproven_scopes_protected() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        let foreign = root.path().join("elsewhere");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // Foreign: cwd outside every registered project.
        add_session_proc(
            &proc,
            500,
            1,
            "devin",
            Some(&foreign),
            300_000,
            (Some(1), Some(1)),
        );
        // Unproven: no cwd link at all.
        add_session_proc(&proc, 600, 1, "claude", None, 300_000, (Some(1), Some(1)));
        let v = sessions_value(&scan);
        let trees = v["trees"].as_array().unwrap();
        let f = trees.iter().find(|t| t["root"]["pid"] == 500).unwrap();
        assert_eq!(f["scope"], "foreign");
        assert_eq!(f["reclaim"]["candidate"], false);
        assert!(f["reclaim"]["protected"]
            .as_str()
            .unwrap()
            .contains("foreign"));
        let u = trees.iter().find(|t| t["root"]["pid"] == 600).unwrap();
        assert_eq!(u["scope"], "unproven");
        assert_eq!(u["reclaim"]["candidate"], false);
        assert_eq!(v["totals"]["candidates"], 0);
    }

    #[test]
    fn sessions_unreadable_store_never_proves_unowned() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        std::fs::create_dir_all(&scan.state_dir).unwrap();
        // A store file that is not sqlite — present but unreadable.
        std::fs::write(scan.state_dir.join("cadence.sqlite3"), b"not a database").unwrap();
        let proc = scan.proc_root.clone();
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(1), Some(1)),
        );
        let v = sessions_value(&scan);
        assert_eq!(v["store"], "unreadable");
        let tree = &v["trees"][0];
        // Absence is unproven → Unknown → protected, never a candidate.
        assert_eq!(tree["agreement"], "unknown");
        assert_eq!(tree["reclaim"]["candidate"], false);
        assert_eq!(v["totals"]["candidates"], 0);
    }

    #[test]
    fn sessions_generation_mismatch_is_fenced() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        add_session_proc(
            &proc,
            50,
            1,
            "bash",
            Some(&repo),
            400_000,
            (Some(1), Some(0)),
        );
        add_session_proc(
            &proc,
            100,
            50,
            "devin",
            Some(&repo),
            400_000,
            (Some(9), Some(2)),
        );
        let conn = fake_registry(&scan.state_dir);
        let gen_old = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let gen_live = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        add_agent(
            &conn,
            "devin-d",
            "pty",
            Some(50),
            Some(gen_old),
            "busy",
            &repo,
            now_epoch(),
        );
        // A live turn token minted under a DIFFERENT generation —
        // the endpoint moved under the registry row.
        add_message(
            &conn,
            "devin-d",
            "running",
            Some(&format!("pty-{gen_live}-{}", "c".repeat(32))),
            None,
        );
        drop(conn);
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["agreement"], "generation-mismatch");
        assert_eq!(tree["reclaim"]["candidate"], false);
        assert!(tree["reclaim"]["protected"]
            .as_str()
            .unwrap()
            .contains("generation"));
    }

    #[test]
    fn sessions_pending_and_progress_surface() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        add_session_proc(
            &proc,
            200,
            1,
            "claude",
            Some(&repo),
            5_000,
            (Some(10), Some(1)),
        );
        let conn = fake_registry(&scan.state_dir);
        let gen = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let now = now_epoch();
        // updated is recent enough for a live claim (proc started
        // ~83min ago); the message timestamp is newer still, so it
        // must win last_progress.
        add_agent(
            &conn,
            "qa-1",
            "managed",
            Some(200),
            Some(gen),
            "busy",
            &repo,
            now - 300.0,
        );
        add_message(&conn, "qa-1", "queued", None, None);
        add_message(&conn, "qa-1", "queued", None, None);
        add_message(
            &conn,
            "qa-1",
            "running",
            Some(&format!("claude-{gen}-{}", "d".repeat(32))),
            None,
        );
        add_message(&conn, "qa-1", "done", None, Some(now + 60.0));
        drop(conn);
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["alias"], "qa-1");
        assert_eq!(tree["state"], "busy");
        assert_eq!(tree["pending"]["queued"], 2);
        assert_eq!(tree["pending"]["running"], 1);
        assert_eq!(tree["last_progress"], now + 60.0);
        // Running token's generation matches the row — still agreed.
        assert_eq!(tree["agreement"], "agreed");
    }

    #[test]
    fn sessions_never_disclose_argv_or_env() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        let dir = add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(1), Some(1)),
        );
        // A bearer secret in argv and env — the census must not read
        // either file, let alone print them.
        std::fs::write(dir.join("cmdline"), "devin\0--token\0S3CR3T-BEARER").unwrap();
        std::fs::write(dir.join("environ"), "GH_TOKEN=S3CR3T-BEARER\0HOME=/u").unwrap();
        let v = sessions_value(&scan);
        let text = serde_json::to_string(&v).unwrap();
        assert!(!text.contains("S3CR3T"), "{text}");
        assert!(!text.contains("cmdline") && !text.contains("environ"));
    }

    #[test]
    fn sessions_partial_metrics_degrade_confidence() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(100), Some(80)),
        );
        // Child whose metric files are absent → partial accounting.
        add_session_proc(&proc, 701, 700, "node", Some(&repo), 300_000, (None, None));
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["pss_missing_pids"], 1);
        assert_eq!(tree["swap_missing_pids"], 1);
        // 83h would be high, but partial accounting caps at medium.
        assert_eq!(tree["reclaim"]["confidence"], "medium");
        assert_eq!(tree["reclaim"]["candidate"], true);
    }

    #[test]
    fn sessions_foreign_uid_never_candidate() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // Unowned, in-scope, old — every candidacy trait except uid:
        // the root belongs to another user.
        let dir = add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(100), Some(80)),
        );
        let euid = unsafe { libc::geteuid() };
        let foreign = euid + 1;
        std::fs::write(
            dir.join("status"),
            format!("Name:\tdevin\nUid:\t{foreign}\t{foreign}\t{foreign}\t{foreign}\n"),
        )
        .unwrap();
        // No registry — "unowned" is a proven fact; uid still fences.
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["agreement"], "process-only");
        assert_eq!(tree["scope"], "project:cadence");
        assert_eq!(tree["root"]["uid"], foreign);
        assert_eq!(tree["reclaim"]["candidate"], false);
        assert!(tree["reclaim"]["protected"]
            .as_str()
            .unwrap()
            .contains(&format!("uid {foreign}")));
        assert_eq!(v["totals"]["foreign_uid"], 1);
        assert_eq!(v["totals"]["candidates"], 0);
    }

    #[test]
    fn sessions_uid_unreadable_is_protected() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // No status file at all → uid unproven → never a candidate.
        add_session_proc(&proc, 700, 1, "devin", Some(&repo), 300_000, (None, None));
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["root"]["uid"], Value::Null);
        assert_eq!(tree["reclaim"]["candidate"], false);
        assert!(tree["reclaim"]["protected"]
            .as_str()
            .unwrap()
            .contains("uid unreadable"));
    }

    #[test]
    fn sessions_scope_rejects_empty_relative_and_root_paths() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // A project whose every repo path is hostile to prefix
        // matching: empty, relative, `/`, and $HOME itself.
        let pm = scan.pm_dir.as_deref().unwrap();
        let dir = pm.join("bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.yaml"),
            format!(
                "key: bad\nprefix: bad-\nrepos:\n  - path: \"\"\n  - path: relative\n  - path: /\n  - path: {}\n",
                scan.home.display()
            ),
        )
        .unwrap();
        let proc = scan.proc_root.clone();
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(1), Some(1)),
        );
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        // Only the pm dir itself survives validation → the repo cwd
        // is outside every proven root → foreign, never a candidate.
        assert_eq!(tree["scope"], "foreign", "{v}");
        assert_eq!(tree["reclaim"]["candidate"], false);
        assert_eq!(v["totals"]["candidates"], 0);
    }

    #[test]
    fn sessions_stale_endpoint_pid_row_fences_tree() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // A live unowned tree under the repo…
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(1), Some(1)),
        );
        // …and a registry row whose endpoint pid is DEAD while its
        // cwd still covers the tree — the pane-respawn shape. The
        // tree may be that agent's session → Unknown, never
        // process-only.
        let conn = fake_registry(&scan.state_dir);
        add_agent(
            &conn,
            "ghost-1",
            "pty",
            Some(9999),
            Some("g1"),
            "idle",
            &repo,
            now_epoch() - 500_000.0,
        );
        drop(conn);
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["agreement"], "unknown", "{v}");
        assert!(tree["agreement_why"].as_str().unwrap().contains("ghost-1"));
        assert_eq!(tree["reclaim"]["candidate"], false);
        // The row itself still shows under records_only.
        let rec = &v["records_only"][0];
        assert_eq!(rec["alias"], "ghost-1");
        assert_eq!(rec["endpoint_state"], "dead");
    }

    #[test]
    fn sessions_recycled_endpoint_pid_is_unknown_not_agreed() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // Live devin root — and a registry row naming its pid, but
        // the row's last write predates this process's start by days:
        // the pid was recycled; it cannot be the recorded endpoint.
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            5_000,
            (Some(1), Some(1)),
        );
        let conn = fake_registry(&scan.state_dir);
        add_agent(
            &conn,
            "qa-1",
            "managed",
            Some(700),
            Some("g1"),
            "idle",
            &repo,
            now_epoch() - 1_000_000.0,
        );
        drop(conn);
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["agreement"], "unknown", "{v}");
        assert!(tree["agreement_why"].as_str().unwrap().contains("reused"));
        assert_eq!(tree["reclaim"]["candidate"], false);
    }

    #[test]
    fn sessions_ambiguous_claim_is_unknown() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            5_000,
            (Some(1), Some(1)),
        );
        let conn = fake_registry(&scan.state_dir);
        for alias in ["a-1", "a-2"] {
            add_agent(
                &conn,
                alias,
                "managed",
                Some(700),
                Some("g1"),
                "idle",
                &repo,
                now_epoch(),
            );
        }
        drop(conn);
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["agreement"], "unknown", "{v}");
        assert!(tree["agreement_why"]
            .as_str()
            .unwrap()
            .contains("ambiguous"));
        assert_eq!(tree["reclaim"]["candidate"], false);
    }

    #[test]
    fn sessions_deleted_cwd_is_unproven_not_in_scope() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        let dir = add_session_proc(&proc, 700, 1, "devin", None, 300_000, (Some(1), Some(1)));
        // The kernel's deleted-marker form: read_link yields the old
        // path plus the literal suffix — must not string-match into
        // scope.
        std::os::unix::fs::symlink(format!("{} (deleted)", repo.display()), dir.join("cwd"))
            .unwrap();
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["root"]["cwd_deleted"], true);
        assert_eq!(tree["scope"], "unproven", "{v}");
        assert_eq!(tree["reclaim"]["candidate"], false);
    }

    #[test]
    fn sessions_foreign_mount_namespace_is_unproven() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        // Our own mnt-ns anchor: proc/self → a fake doctor pid dir.
        let selfdir = proc.join("900");
        std::fs::create_dir_all(selfdir.join("ns")).unwrap();
        std::os::unix::fs::symlink("mnt:[1111]", selfdir.join("ns/mnt")).unwrap();
        std::os::unix::fs::symlink(&selfdir, proc.join("self")).unwrap();
        // In-scope cwd but a different mnt ns → not comparable.
        let a = add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(1), Some(1)),
        );
        std::fs::create_dir_all(a.join("ns")).unwrap();
        std::os::unix::fs::symlink("mnt:[2222]", a.join("ns/mnt")).unwrap();
        // Same-ns control → normal classification.
        let b = add_session_proc(
            &proc,
            600,
            1,
            "claude",
            Some(&repo),
            300_000,
            (Some(1), Some(1)),
        );
        std::fs::create_dir_all(b.join("ns")).unwrap();
        std::os::unix::fs::symlink("mnt:[1111]", b.join("ns/mnt")).unwrap();
        let v = sessions_value(&scan);
        let trees = v["trees"].as_array().unwrap();
        let foreign_ns = trees.iter().find(|t| t["root"]["pid"] == 700).unwrap();
        assert_eq!(foreign_ns["root"]["ns_foreign"], true);
        assert_eq!(foreign_ns["scope"], "unproven", "{v}");
        assert_eq!(foreign_ns["reclaim"]["candidate"], false);
        let same_ns = trees.iter().find(|t| t["root"]["pid"] == 600).unwrap();
        assert_eq!(same_ns["root"]["ns_foreign"], false);
        assert_eq!(same_ns["scope"], "project:cadence");
    }

    #[test]
    fn sessions_ppid_cycle_terminates() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let proc = scan.proc_root.clone();
        // A ppid cycle among session comms — each has a session
        // ancestor, so neither is a root and the walk must end.
        add_session_proc(&proc, 800, 801, "devin", None, 5_000, (Some(1), Some(0)));
        add_session_proc(&proc, 801, 800, "claude", None, 5_000, (Some(1), Some(0)));
        let v = sessions_value(&scan);
        assert_eq!(v["trees"].as_array().unwrap().len(), 0, "{v}");
    }

    #[test]
    fn sessions_metrics_cap_marks_truncated() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(1), Some(1)),
        );
        for pid in 800..(800 + MAX_METRIC_PIDS as u32 + 3) {
            add_session_proc(
                &proc,
                pid,
                700,
                "node",
                Some(&repo),
                300_000,
                (Some(1), Some(0)),
            );
        }
        let v = sessions_value(&scan);
        let tree = &v["trees"][0];
        assert_eq!(tree["metrics_truncated"], true);
        // Members list stays complete — only the metric pass is capped.
        assert_eq!(tree["procs"], MAX_METRIC_PIDS + 4);
        assert_eq!(tree["reclaim"]["confidence"], "medium");
        // A truncated tree in the candidate list means the aggregate
        // is an estimate, not an upper bound — the flag must flip.
        assert_eq!(tree["reclaim"]["candidate"], true);
        assert_eq!(v["totals"]["candidate_sums_upper_bound"], false);
    }

    #[test]
    fn sessions_unreadable_store_warns_and_exits_nonzero() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        std::fs::create_dir_all(&scan.state_dir).unwrap();
        std::fs::write(scan.state_dir.join("cadence.sqlite3"), b"not a database").unwrap();
        let report = run(&scan);
        let c = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "sessions")
            .unwrap();
        // Evidence failure must never exit 0 — a watchdog loop
        // keying on the exit code learns the store could not be read.
        assert_ne!(c["level"], "ok", "{c}");
        assert!(exit_code(&report) >= 1);
    }

    #[test]
    fn sessions_candidate_caveat_is_visible() {
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
        let proc = scan.proc_root.clone();
        add_session_proc(
            &proc,
            700,
            1,
            "devin",
            Some(&repo),
            300_000,
            (Some(100), Some(80)),
        );
        // A *readable* registry (zero rows) — the candidate check still
        // reports ok, pinning the warn-only-on-evidence-failure rule so
        // nobody "fixes" it back.
        drop(fake_registry(&scan.state_dir));
        let report = run(&scan);
        let c = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "sessions")
            .unwrap();
        assert_eq!(c["level"], "ok", "{c}");
        // The caveat must reach the text render — the `remedy` field
        // holding it is not the point; the printed line is.
        let text = render(&report);
        assert!(
            text.contains("separately authorised phase"),
            "caveat missing from render:\n{text}"
        );
    }

    #[test]
    fn pm_yaml_host_slot_keys_parse() {
        let root = TempDir::new().unwrap();
        let pm = root.path().join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  build_slots: 5\n  suite_slots: 2\n  \
             jobs_per_lane: 8\n  starve_secs: 300\n  \
             priority_lanes: [qa-1, qa-2]\n  load_warn_ratio: 1.5\n  \
             io_stall_fail_pct: 45\n",
        )
        .unwrap();
        let o = host_overrides(&pm).unwrap();
        assert_eq!(o.build_slots, Some(5));
        assert_eq!(o.suite_slots, Some(2));
        assert_eq!(o.jobs_per_lane, Some(8));
        assert_eq!(o.starve_secs, Some(300));
        assert_eq!(o.priority_lanes.as_deref().unwrap().len(), 2);
        assert_eq!(o.load_warn_ratio, Some(1.5));
        // …and it resolves through to the threshold.
        let t = Thresholds::resolve(Some(o));
        assert_eq!(t.load_warn_ratio, Some(1.5));
        assert_eq!(t.io_stall_fail_pct, 45.0);
    }

    // ---------- load (CAD-113) ----------

    /// Fabricate `loadavg` + `pressure/io` under the scan's proc root.
    fn proc_load(scan: &Scan, load1: f64, io_avg10: Option<f64>) {
        std::fs::create_dir_all(scan.proc_root.join("pressure")).unwrap();
        std::fs::write(
            scan.proc_root.join("loadavg"),
            format!("{load1} 1.00 1.00 1/100 999\n"),
        )
        .unwrap();
        let io = match io_avg10 {
            Some(v) => format!("some avg10={v} avg60=0.00 avg300=0.00 total=1\n"),
            None => String::new(),
        };
        std::fs::write(scan.proc_root.join("pressure/io"), io).unwrap();
    }

    #[test]
    fn load_check_levels_and_slot_detail() {
        let root = TempDir::new().unwrap();
        let mut scan = fake_scan(&root);
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        // Calm host: level ok, detail still names the slot queue.
        scan.slots = Some(json!({
            "pools": {
                "build": {"capacity": 3, "held": [{"a": 1}, {"a": 2}]},
                "suite": {"capacity": 1, "held": []},
            },
            "waiting": [{"wait_secs": 252.0}],
        }));
        proc_load(&scan, 0.5, Some(2.0));
        let c = check_load(&scan);
        assert_eq!(c.level, Level::Ok, "{}", c.detail);
        assert!(
            c.detail.contains("slots 2/3 build 0/1 suite"),
            "{}",
            c.detail
        );
        assert!(c.detail.contains("longest 4m12s"), "{}", c.detail);
        // An explicit warn ratio pins the bands regardless of cpus.
        scan.thresholds.load_warn_ratio = Some(1.0);
        // Warn band: load1 above cpus but under 2x.
        proc_load(&scan, cpus * 1.5, Some(10.0));
        let c = check_load(&scan);
        assert_eq!(c.level, Level::Warn, "{}", c.detail);
        assert!(c.remedy.contains("build-slot status"));
        // Fail band: io stall alone can carry it.
        proc_load(&scan, 0.5, Some(70.0));
        let c = check_load(&scan);
        assert_eq!(c.level, Level::Fail, "{}", c.detail);
        // Load alone over 2x also fails.
        proc_load(&scan, cpus * 2.5, Some(0.0));
        let c = check_load(&scan);
        assert_eq!(c.level, Level::Fail, "{}", c.detail);
        // Unset, the warn line derives from the slot plan — the
        // farm's own (3+1)×4 jobs on this box: warn only above it.
        scan.thresholds.load_warn_ratio = None;
        let derived = ((3.0 + 1.0) * 4.0 * 1.25 / cpus).max(1.0);
        // The fixture's slot config (3/1/4) equals the defaults, so
        // the derived ratio matches either way; below it → ok.
        proc_load(&scan, cpus * derived * 0.9, Some(2.0));
        let c = check_load(&scan);
        assert_eq!(c.level, Level::Ok, "below plan: {}", c.detail);
        proc_load(&scan, cpus * derived * 1.5, Some(2.0));
        let c = check_load(&scan);
        assert_eq!(c.level, Level::Warn, "above plan: {}", c.detail);
        // Unreachable daemon reports, never penalises.
        scan.slots = None;
        proc_load(&scan, 0.5, Some(2.0));
        let c = check_load(&scan);
        assert_eq!(c.level, Level::Ok);
        assert!(c.detail.contains("daemon unreachable"), "{}", c.detail);
        // Neither file exists → skipped ok.
        let root = TempDir::new().unwrap();
        let scan = fake_scan(&root);
        let c = check_load(&scan);
        assert_eq!(c.level, Level::Ok);
        assert!(c.value["skipped"].as_bool().unwrap_or(false));
    }
}
