//! CAD-561: the board's Update card.
//!
//! Settings shows the current version and, from the last check,
//! "Update available · N changes" with the summary; the **Update**
//! button starts the same pipeline as `cadence update` — as a detached
//! helper process that outlives this board (CAD-561 r2). The switch
//! restarts the board, so a pipeline running in-process would SIGTERM
//! its own process before `ui start`; the helper instead appends its
//! progress and its result to `<state>/update-progress.jsonl`, and this
//! board — and the one that replaces it — reads that file back. The
//! route class is [`RouteClass::OperatorOnly`], so the button carries
//! the same proof as every other operator write
//! ([`super::operator::admit`] plus [`super::home::prove_operator_peer`])
//! — an agent's session can never start an update. A banner shows while
//! an update drains, from the same `update_status`/`health` view the CLI
//! reads.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::{coded_response, json_response, HttpResp, ServeOpts};
use crate::client;
use crate::error::{Error, Result};
use crate::ui::home::rpc_err;
use crate::update::{self, PendingUpdate, RestartOutcome, UpdateHost, Waiter};
use crate::upgrade::{self, Layout, ReleaseSource};

/// The board process's cached update check: the read-only `gh` query
/// behind "Update available · N changes". The run itself lives in the
/// helper's progress file, not here — the board that started it does
/// not survive the switch, and the one that does reads the file.
#[derive(Default)]
struct BoardUpdate {
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

/// How long a cached check stays fresh before the card refreshes it in
/// the background.
const CHECK_EVERY: f64 = 600.0;

/// `GET /api/update` — the card's whole view: the current release, the
/// last check, the running update's progress (read from the helper's
/// log, so it survives this board's own replacement), and the drain
/// state.
pub(super) fn get(state_dir: &Path) -> HttpResp {
    maybe_refresh_check(state_dir);
    let layout = layout();
    let current = layout.as_ref().map(current).unwrap_or(Value::Null);
    let (pending, waiting) = pending(state_dir);
    let run = update::read_run_log(state_dir);
    adopt_finished_check(&run);
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
        "running": run.running,
        "lines": run.lines,
        "result": run.result,
        "error": run.error,
        "pending": pending,
        "waiting": waiting,
    }))
}

/// A finished run's own check is the newest the card can have: adopt it
/// (with the run's end as `checked_at`) so a board restarted by the
/// update shows the post-update state without waiting for its own
/// background check. Older than the cached check: left alone.
fn adopt_finished_check(run: &update::RunLogView) {
    let Some(finished_at) = run.finished_at else {
        return;
    };
    let Some(check) = run
        .result
        .as_ref()
        .and_then(|report| report.get("check"))
        .filter(|check| check.is_object())
    else {
        return;
    };
    let stale = {
        let checked_at = BOARD_UPDATE.checked_at.lock().unwrap();
        checked_at.is_none_or(|at| finished_at > at)
    };
    if !stale {
        return;
    }
    *BOARD_UPDATE.check.lock().unwrap() = Some(check.clone());
    *BOARD_UPDATE.checked_at.lock().unwrap() = Some(finished_at);
}

/// The card fills itself in: a check is kicked in the background when
/// the cached one is missing or older than [`CHECK_EVERY`], so Settings
/// shows "Update available · N changes" without a click. Read-only (gh
/// queries), single-flight, and never a caller's input.
fn maybe_refresh_check(state_dir: &Path) {
    let fresh = BOARD_UPDATE
        .checked_at
        .lock()
        .unwrap()
        .is_some_and(|at| crate::rollout::unix_now() - at < CHECK_EVERY);
    if fresh || BOARD_UPDATE.checking.swap(true, Ordering::SeqCst) {
        return;
    }
    let state_dir = state_dir.to_path_buf();
    std::thread::spawn(move || {
        if let Ok(report) = run_check(&state_dir) {
            *BOARD_UPDATE.check.lock().unwrap() = Some(report);
            *BOARD_UPDATE.checked_at.lock().unwrap() = Some(crate::rollout::unix_now());
        }
        BOARD_UPDATE.checking.store(false, Ordering::SeqCst);
    });
}

/// `POST /api/update/check` — run the real check (gh) now and cache it.
pub(super) fn check_now(state_dir: &Path) -> HttpResp {
    let outcome = run_check(state_dir);
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
    };
    Ok(update::check(&host)?.to_json())
}

/// `POST /api/update` — start the pipeline as a detached helper that
/// outlives this board (CAD-561 r2). Returns at once; the card polls
/// `GET /api/update`, which reads the helper's progress file — the
/// board that answers after the switch reads the same file.
pub(super) fn start(state_dir: &Path, opts: &ServeOpts) -> HttpResp {
    if update::read_run_log(state_dir).running {
        return coded_response(409, "update_running", "an update is already running", None);
    }
    // The board's own binary in production; a test's fake helper.
    let exe = match &opts.update_helper {
        Some(path) => path.clone(),
        None => match std::env::current_exe() {
            Ok(path) => path,
            Err(e) => {
                return rpc_err(
                    &Error::internal(format!("the board's own binary: {e}")),
                    "update",
                )
            }
        },
    };
    let log_path = update::progress_file(state_dir);
    let log = match update::open_private(&log_path, true, false) {
        Ok(file) => file,
        Err(e) => return rpc_err(&e, "update"),
    };
    let stdout = match log.try_clone() {
        Ok(file) => file,
        Err(e) => {
            return rpc_err(
                &Error::internal(format!("{}: {e}", log_path.display())),
                "update",
            )
        }
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--state-dir")
        .arg(state_dir)
        .args(["update", "--as"])
        .arg(super::UI_ACTOR)
        .arg("--progress")
        .arg(&log_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(log));
    // The helper is the operator's, not an agent's: no ambient alias and
    // no fixture seam assertion rides into it (CAD-482).
    cmd.env_remove("CADENCE_ALIAS");
    cmd.env_remove(crate::test_seam::AS_ENV);
    // Its own session: the restart this helper runs stops the board by
    // pid, and a helper in the board's session would be a candidate for
    // any group signal; detached, it outlives the board entirely.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    match crate::reaper::spawn(&mut cmd) {
        Ok(mut child) => {
            let pid = child.id();
            // This board is the helper's parent: reap it when it exits,
            // or its zombie pid keeps reading as a live run.
            let state_dir = state_dir.to_path_buf();
            std::thread::spawn(move || {
                let status = child.wait();
                // A helper that dies before it opens the run log (the
                // CLI gate or the re-entry lock refused it) leaves no
                // terminal record while this board already answered
                // `started`: write the record the card reads (CAD-561 r3).
                if let Ok(status) = status {
                    update::run_log_never_started(&state_dir, status);
                }
            });
            json_response(json!({"started": true, "pid": pid}))
        }
        Err(e) => rpc_err(
            &Error::internal(format!("could not start {}: {e}", exe.display())),
            "update",
        ),
    }
}

/// The board's [`UpdateHost`] — used for the read-only `check` only:
/// the pipeline itself runs in the detached helper (CAD-561 r2). The
/// pipeline methods refuse rather than act, so a future change cannot
/// quietly put the pipeline back inside the process its own restart
/// stops.
struct BoardHost {
    state_dir: PathBuf,
    layout: Layout,
    source: upgrade::Gh,
}

impl BoardHost {
    fn not_the_pipeline() -> Error {
        Error::internal(
            "the board's check host never runs the update pipeline — the board starts \
             a detached `cadence update` helper instead (CAD-561 r2)",
        )
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
    fn progress(&self, _line: &str) {}
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
    fn set_pending(&self, _pending: Option<&PendingUpdate>) -> Result<()> {
        Err(Self::not_the_pipeline())
    }
    fn pending(&self) -> Option<PendingUpdate> {
        None
    }
    fn set_drain(&self, _on: bool) -> Result<()> {
        Err(Self::not_the_pipeline())
    }
    fn restart(&self, _binary: &Path) -> Result<RestartOutcome> {
        Err(Self::not_the_pipeline())
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
    fn board_running(&self) -> bool {
        crate::ui::detached_pid(&self.state_dir).is_some()
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
