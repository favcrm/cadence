//! CAD-561: the board's Update card.
//!
//! Settings shows the current version and, from the last check,
//! "Update available · N changes" with the summary; the **Update**
//! button runs the same pipeline as `cadence update` in this process
//! and streams its progress. The route class is
//! [`RouteClass::OperatorOnly`], so the button carries the same proof
//! as every other operator write ([`super::operator::admit`] plus
//! [`super::home::prove_operator_peer`]) — an agent's session can never
//! start an update. A banner shows while an update drains, from the
//! same `update_status`/`health` view the CLI reads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::{coded_response, json_response, HttpResp, ServeOpts};
use crate::client;
use crate::error::{Error, Result};
use crate::ui::home::rpc_err;
use crate::update::{self, Options, PendingUpdate, UpdateHost, Waiter};
use crate::upgrade::{self, Layout, ReleaseSource};

/// The board process's one update slot: at most one runs at a time,
/// and its progress is readable by every request.
#[derive(Default)]
struct BoardUpdate {
    running: AtomicBool,
    lines: Mutex<Vec<String>>,
    result: Mutex<Option<Value>>,
    error: Mutex<Option<String>>,
    check: Mutex<Option<Value>>,
    checked_at: Mutex<Option<f64>>,
    checking: AtomicBool,
}

static BOARD_UPDATE: LazyLock<BoardUpdate> = LazyLock::new(BoardUpdate::default);

/// The layout the board updates: the same default the CLI uses
/// (`~/.local/bin/cadence` and the releases dir read off it).
fn layout() -> Result<Layout> {
    Layout::detect(None, None)
}

fn version_of(layout: &Layout, sha: &str) -> Option<String> {
    let text = std::fs::read_to_string(layout.release_dir(sha).join(upgrade::MANIFEST)).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    (manifest["source_sha"].as_str() == Some(sha))
        .then(|| format!("{}+{sha}", env!("CARGO_PKG_VERSION")))
}

/// The linked release, as the card's "current version".
fn current(layout: &Layout) -> Value {
    match upgrade::current(layout) {
        Ok(upgrade::Current::Link { sha: Some(sha), .. }) => {
            json!({"sha": sha, "version": version_of(layout, &sha)})
        }
        _ => json!({"sha": Value::Null, "version": Value::Null}),
    }
}

/// The daemon's pending update + waiters, or empty when it is not
/// reachable (nothing is draining then).
fn pending(state_dir: &Path) -> (Value, Vec<Value>) {
    match client::rpc(state_dir, "update_status", json!({})) {
        Ok(status) => (
            status["pending_update"].clone(),
            status["waiting"].as_array().cloned().unwrap_or_default(),
        ),
        Err(_) => (Value::Null, Vec::new()),
    }
}

/// `GET /api/update` — the card's whole view: the current release, the
/// last check, the running update's progress, and the drain state.
pub(super) fn get(state_dir: &Path) -> HttpResp {
    let layout = layout();
    let current = layout.as_ref().map(current).unwrap_or(Value::Null);
    let (pending, waiting) = pending(state_dir);
    let check = BOARD_UPDATE.check.lock().unwrap().clone();
    let checked_at = *BOARD_UPDATE.checked_at.lock().unwrap();
    let update_available = check
        .as_ref()
        .is_some_and(|c| c["up_to_date"].as_bool() == Some(false));
    json_response(json!({
        "current": current,
        "check": check,
        "checked_at": checked_at,
        "checking": BOARD_UPDATE.checking.load(Ordering::SeqCst),
        "update_available": update_available,
        "change_count": check.as_ref().and_then(|c| c["change_count"].as_u64()).unwrap_or(0),
        "changes": check.as_ref().map(|c| c["changes"].clone()).unwrap_or(json!([])),
        "migration": check.as_ref().and_then(|c| c["schema"]["migration"].as_bool()).unwrap_or(false),
        "blockers": check.as_ref().map(|c| c["blockers"].clone()).unwrap_or(json!([])),
        "running": BOARD_UPDATE.running.load(Ordering::SeqCst),
        "lines": BOARD_UPDATE.lines.lock().unwrap().clone(),
        "result": BOARD_UPDATE.result.lock().unwrap().clone(),
        "error": BOARD_UPDATE.error.lock().unwrap().clone(),
        "pending": pending,
        "waiting": waiting,
    }))
}

/// `POST /api/update/check` — run the real check (gh) now and cache it.
pub(super) fn check_now(state_dir: &Path) -> HttpResp {
    if BOARD_UPDATE.checking.swap(true, Ordering::SeqCst) {
        return coded_response(409, "check_running", "a check is already running", None);
    }
    let outcome = run_check(state_dir);
    BOARD_UPDATE.checking.store(false, Ordering::SeqCst);
    match outcome {
        Ok(report) => {
            *BOARD_UPDATE.check.lock().unwrap() = Some(report.clone());
            *BOARD_UPDATE.checked_at.lock().unwrap() = Some(crate::rollout::unix_now());
            json_response(report)
        }
        Err(err) => rpc_err(&err, "update check"),
    }
}

fn run_check(state_dir: &Path) -> Result<Value> {
    let layout = layout()?;
    let host = BoardHost {
        state_dir: state_dir.to_path_buf(),
        layout,
        source: upgrade::Gh::new(upgrade::DEFAULT_REPO),
        pending: Mutex::new(None),
    };
    Ok(update::check(&host)?.to_json())
}

/// `POST /api/update` — start the pipeline in this process. Returns at
/// once; the card polls `GET /api/update` for the progress lines.
pub(super) fn start(state_dir: &Path, opts: &ServeOpts) -> HttpResp {
    if BOARD_UPDATE.running.swap(true, Ordering::SeqCst) {
        return coded_response(409, "update_running", "an update is already running", None);
    }
    BOARD_UPDATE.lines.lock().unwrap().clear();
    *BOARD_UPDATE.result.lock().unwrap() = None;
    *BOARD_UPDATE.error.lock().unwrap() = None;
    let state_dir = state_dir.to_path_buf();
    let opts = opts.clone();
    std::thread::spawn(move || {
        let outcome = run_update(&state_dir, &opts);
        BOARD_UPDATE.running.store(false, Ordering::SeqCst);
        match outcome {
            Ok(report) => {
                *BOARD_UPDATE.result.lock().unwrap() = Some(report.to_json());
                // The check the update just made is the new cache.
                *BOARD_UPDATE.check.lock().unwrap() = Some(report.check.to_json());
                *BOARD_UPDATE.checked_at.lock().unwrap() = Some(crate::rollout::unix_now());
            }
            Err(err) => *BOARD_UPDATE.error.lock().unwrap() = Some(err.to_string()),
        }
    });
    json_response(json!({"started": true}))
}

fn run_update(state_dir: &Path, _opts: &ServeOpts) -> Result<update::RunReport> {
    let layout = layout()?;
    let host = BoardHost {
        state_dir: state_dir.to_path_buf(),
        layout,
        source: upgrade::Gh::new(upgrade::DEFAULT_REPO),
        pending: Mutex::new(None),
    };
    let report = update::run(&host, &Options::default())?;
    Ok(report)
}

/// The board's [`UpdateHost`]. The operator authority is the route's
/// proof; this host makes the same daemon calls the CLI makes, from
/// this process's own connection — which the daemon attributes exactly
/// as it does for the board's other relayed operator writes.
struct BoardHost {
    state_dir: PathBuf,
    layout: Layout,
    source: upgrade::Gh,
    /// The marker this process last recorded, for the drain re-assertion.
    pending: Mutex<Option<PendingUpdate>>,
}

impl BoardHost {
    fn label(&self) -> String {
        super::UI_ACTOR.to_string()
    }
}

impl UpdateHost for BoardHost {
    fn state_dir(&self) -> &Path {
        &self.state_dir
    }
    fn layout(&self) -> &Layout {
        &self.layout
    }
    fn source(&self) -> &dyn ReleaseSource {
        &self.source
    }
    fn identity(&self) -> &str {
        super::UI_ACTOR
    }
    fn progress(&self, line: &str) {
        BOARD_UPDATE.lines.lock().unwrap().push(line.to_string());
    }
    fn waiters(&self) -> Result<Vec<Waiter>> {
        let status = match client::rpc(&self.state_dir, "update_status", json!({})) {
            Ok(status) => status,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(status["waiting"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        Some(Waiter {
                            alias: r["alias"].as_str()?.to_string(),
                            message: r["message"].as_str().unwrap_or_default().to_string(),
                            state: r["state"].as_str().unwrap_or_default().to_string(),
                            age_secs: r["age_secs"].as_u64().unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
    fn set_pending(&self, pending: Option<&PendingUpdate>) -> Result<()> {
        *self.pending.lock().unwrap() = pending.cloned();
        match pending {
            Some(pending) => update::write_pending(&self.state_dir, pending),
            None => {
                update::clear_pending(&self.state_dir);
                Ok(())
            }
        }
    }
    fn pending(&self) -> Option<PendingUpdate> {
        self.pending.lock().unwrap().clone()
    }
    fn set_drain(&self, on: bool) -> Result<()> {
        let mut params = json!({"on": on, "label": self.label()});
        if on {
            if let Some(pending) = self.pending.lock().unwrap().clone() {
                params["target"] = json!(pending.target);
                params["phase"] = json!(pending.phase);
                params["from"] = json!(pending.from);
                params["since"] = json!(pending.since);
            }
        }
        match client::rpc(&self.state_dir, "update_drain", params) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().starts_with("Daemon is not reachable") => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn restart(&self, binary: &Path) -> Result<()> {
        let mut cmd = std::process::Command::new(binary);
        cmd.arg("--state-dir")
            .arg(&self.state_dir)
            .args(["daemon", "restart", "--ui", "--as"])
            .arg(self.label())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let out = crate::reaper::spawn(&mut cmd)
            .and_then(|child| child.wait_with_output())
            .map_err(|e| Error::internal(format!("could not run {}: {e}", binary.display())))?;
        if !out.status.success() {
            return Err(Error::rejected(format!(
                "the restart on {} failed (exit {}): {}",
                binary.display(),
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }
    fn daemon_build(&self) -> Result<Option<String>> {
        match client::rpc(&self.state_dir, "daemon_info", json!({})) {
            Ok(info) => Ok(info["build_commit"].as_str().map(str::to_string)),
            Err(_) => Ok(None),
        }
    }
    fn board_build(&self) -> Result<Option<String>> {
        match crate::ui::health(&self.state_dir) {
            None => Ok(None),
            Some((_port, body)) => Ok(serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["build"].as_str().map(str::to_string))),
        }
    }
    fn now(&self) -> f64 {
        crate::rollout::unix_now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The draining banner's data, for every page (not just Settings): the
/// pending update and what it waits on, or null.
pub(super) fn banner(state_dir: &Path) -> Value {
    let (pending, waiting) = pending(state_dir);
    if pending.is_null() {
        return Value::Null;
    }
    json!({"pending": pending, "waiting": waiting, "count": waiting.len()})
}

/// `GET /api/update/banner` — cheap enough for the SPA to poll.
pub(super) fn banner_get(state_dir: &Path) -> HttpResp {
    json_response(banner(state_dir))
}
