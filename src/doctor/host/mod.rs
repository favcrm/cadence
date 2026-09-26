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
//! provider WALs itself while their provider idles (CAD-132). The
//! `tailnet` check runs the tailnet sign-in proof's host-side rungs
//! up front — plus a live board's `operator_latched`, read off its
//! `/api/meta` — and prints the whole remedy chain in order, so one
//! sign-in link is spent after the fixes, not one per refusal
//! (CAD-509).
//!
//! Every check reports `ok | warn | fail` with the measured value, the
//! threshold it was compared against, and a `remedy` — the exact
//! command an operator would run, except `task-targets`, whose remedy
//! is a read-only inventory note and never a deletion. Nothing here
//! writes, signals or deletes: filesystem reads, `/proc` walks and a
//! handful of read-only `git` probes are the whole surface. The exit
//! code is the worst level: 0 all ok, 1 any warn, 2 any fail.

// CAD-536: `cadence doctor host` — check orchestration, shared
// measurement types and helpers. All code here is moved verbatim
// from src/doctor/host.rs; checks live in <check>.rs files and
// shared measurement machinery in util.rs.

mod agent_uid;
mod config;
mod disk;
mod layout;
mod load;
mod memory;
mod orphans;
mod pane_identity;
mod pipes;
mod processes;
mod provider_state;
mod sessions;
mod tailnet;
mod task_targets;
mod temp_dirs;
#[cfg(test)]
mod tests;
mod util;
mod worktrees;

use crate::error::Result;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

const GIT_TIMEOUT: Duration = Duration::from_secs(15);

const GIB: u64 = 1 << 30;

const MIB: u64 = 1 << 20;

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
    /// CAD-199 — read by the daemon's agent-gc timer, not by
    /// `Thresholds`. Unset (the default) keeps the timer OFF; set, the
    /// daemon sweeps dead agent registry rows idle longer than this
    /// many seconds (raised to a 7-day floor) at most once an hour.
    /// Records only: frees no memory and no disk, and a removed agent
    /// can no longer be resumed.
    pub agent_gc_older_than_secs: Option<u64>,
    /// CAD-96 — read by the daemon's idle auto-stop timer, not by
    /// `Thresholds`. Unset is the built-in 3600 (ON); `0` turns it off
    /// for every provider without a `_by_provider` entry. A resumable
    /// stop: `cadence agent resume <alias>` brings the agent back.
    pub auto_stop_idle_secs: Option<u64>,
    /// Per-provider idle bound (`claude: 7200`, `codex: 0` = off for
    /// that provider) — overrides `auto_stop_idle_secs`.
    pub auto_stop_idle_secs_by_provider: Option<std::collections::BTreeMap<String, u64>>,
    /// CAD-339 — read by the daemon's report router, not by
    /// `Thresholds`. A worker's open question reaches the master once no
    /// PM has answered it for this many seconds (unset = 900; `0` routes
    /// it at once).
    pub question_escalate_after_secs: Option<u64>,
    /// CAD-556 — read at `agent_register` for `pi/managed`: when true,
    /// a pi worker joins Landlock-confined unless its params say
    /// `confine` explicitly (`join … pi --confine`/`--no-confine` win).
    /// Unset is off — confinement stays opt-in until dogfooded.
    pub confine_pi_workers: Option<bool>,
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
    /// tailscaled's LocalAPI socket the `tailnet` check reads
    /// ([`crate::tailnet_proof`]); `None` reads the default paths.
    /// Never set from the command line — tests inject a fixture.
    pub tailscaled_socket: Option<PathBuf>,
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
            tailscaled_socket: None,
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

/// `pm.yaml [hosted]` — the CAD-538 hosted-lifecycle table (`lease`,
/// `lease_ttl_secs`, `lease_renew_secs`, `flush_timeout_secs`; see
/// `crate::lease`). Same contract as [`read_host_overrides`]: `Ok(None)`
/// when absent, `Err` naming the culprit when present but unusable —
/// a daemon that asked to lease never starts unleased on a typo.
pub(crate) fn read_hosted_overrides(
    pm_dir: &Path,
) -> std::result::Result<Option<crate::lease::Hosted>, String> {
    let Ok(text) = std::fs::read_to_string(pm_dir.join("pm.yaml")) else {
        return Ok(None);
    };
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| format!("pm.yaml is not valid YAML: {e}"))?;
    let Some(hosted) = yaml.get("hosted") else {
        return Ok(None);
    };
    serde_yaml::from_value(hosted.clone())
        .map(Some)
        .map_err(|e| {
            let culprit = hosted.as_mapping().and_then(|m| {
                m.iter().find_map(|(k, v)| {
                    let mut one = serde_yaml::Mapping::new();
                    one.insert(k.clone(), v.clone());
                    serde_yaml::from_value::<crate::lease::Hosted>(serde_yaml::Value::Mapping(one))
                        .is_err()
                        .then(|| k.as_str().unwrap_or("?").to_string())
                })
            });
            match culprit {
                Some(key) => format!("pm.yaml [hosted] {key}: {e}"),
                None => format!("pm.yaml [hosted]: {e}"),
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
        check_pane_identity(scan),
        check_orphans(scan),
        check_temp_dirs(scan),
        check_task_targets(scan),
        check_worktrees(scan),
        check_load(scan),
        check_config(scan),
        check_layout(scan),
        check_tailnet(scan),
        check_agent_uid(),
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
                // A kill remedy is one line per pid — continuation
                // lines indent under the first.
                for (i, line) in remedy.lines().enumerate() {
                    if i == 0 {
                        out.push_str(&format!("       remedy: {line}\n"));
                    } else {
                        out.push_str(&format!("               {line}\n"));
                    }
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

// Re-exports — the public surface of `crate::doctor::host`
// is unchanged by the split; `pub(super)` lines re-bind moved
// helpers at module scope (CAD-536).
#[allow(unused_imports)]
use agent_uid::check_agent_uid;
#[allow(unused_imports)]
use config::check_config;
#[allow(unused_imports)]
use disk::{check_disk, fs_level};
#[allow(unused_imports)]
use layout::check_layout;
#[allow(unused_imports)]
use load::check_load;
#[allow(unused_imports)]
use memory::check_memory;
pub use orphans::redact_argv;
#[allow(unused_imports)]
use orphans::{check_orphans, cmdline, deleted_worktree, probe_pid, Orphan, Probe};
#[allow(unused_imports)]
use pane_identity::check_pane_identity;
#[allow(unused_imports)]
use pipes::{check_pipes, scan_pipes, PipeStats};
#[allow(unused_imports)]
use processes::check_processes;
#[allow(unused_imports)]
pub(crate) use provider_state::wal_sibling;
#[allow(unused_imports)]
use provider_state::{check_provider_state, store_level};
#[allow(unused_imports)]
use sessions::{check_sessions, MAX_METRIC_PIDS};
#[allow(unused_imports)]
use tailnet::check_tailnet;
#[allow(unused_imports)]
use task_targets::{
    check_task_targets, legacy_task_target_name, lexical_kind, probe_lock_file, try_lock_probe,
    LexicalKind, LockBit, TASK_TARGET_ROW_CAP,
};
#[allow(unused_imports)]
use temp_dirs::check_temp_dirs;
#[allow(unused_imports)]
pub(crate) use util::find_wals;
#[allow(unused_imports)]
pub(crate) use util::has_secret_prefix;
#[allow(unused_imports)]
pub(crate) use util::wal_roots;
#[allow(unused_imports)]
pub(crate) use util::Census;
#[allow(unused_imports)]
pub(crate) use util::FsFree;
#[allow(unused_imports)]
pub(crate) use util::WalRoot;
#[allow(unused_imports)]
pub(crate) use util::WalScan;
#[allow(unused_imports)]
pub(crate) use util::SECRET_PREFIXES;
#[allow(unused_imports)]
use util::{
    census_of, comm_family, dir_size, dir_size_limited, file_locked, group_line, kill_lines,
    kill_remedy, pid_age_secs, proc_census, proc_stat, proc_uptime, read_u64_file,
    redact_argv_parts, repo_root, shell_quote, stale_worktrees, top_groups, GroupAgg, OldestProc,
    ProcStat, REDACTED,
};
pub use util::{reclaim_plan, render_reclaim};
#[allow(unused_imports)]
use worktrees::check_worktrees;
