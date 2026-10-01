//! `/api/setup` — the wizard's checks (CAD-327). Detect-only: nothing is
//! applied, started or written from the board. Plus the board read
//! model's cost meters (`read_model_stats`, test support) and the write
//! lock `serve` serializes every write route under.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tiny_http::Request;

use super::serve::{err_response, json_response};
use super::write_path::{guard_fail, tailnet_host, HttpResp};
use super::{operator, read_model, ServeOpts};
use crate::error::{Error, Result};

/// How long one run of the setup checks answers `GET /api/setup`.
const SETUP_FRESH_FOR: Duration = Duration::from_secs(60);
/// A `?fresh=1` re-check younger than this is answered from the last
/// run — a held-down button cannot keep provider CLIs spawning.
const SETUP_MIN_RECHECK: Duration = Duration::from_secs(5);

/// The last run and when it finished. The lock is held while the
/// checks run, so concurrent requests share one run instead of each
/// spawning the provider probes.
static SETUP_CACHE: std::sync::Mutex<Option<(Instant, u64, Value)>> = std::sync::Mutex::new(None);

/// Runs of the setup checks this process made — tests pin that a
/// refused or reused request spawns no probe.
static SETUP_RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[doc(hidden)]
pub fn setup_runs() -> u64 {
    SETUP_RUNS.load(std::sync::atomic::Ordering::SeqCst)
}

/// `/api/setup` is the operator's, on the host: it shows HOME's layout,
/// the installed CLIs and their sign-in state, and the daemon's pid and
/// socket, and a visit spawns the provider probes. A read-only board, a
/// request through the tailnet (proven or not) and a peer that is not
/// loopback are refused — before anything runs.
pub(crate) fn setup_refusal(request: &Request, opts: &ServeOpts) -> Option<HttpResp> {
    const WHY: &str = "setup runs on the host — open the board on 127.0.0.1 there";
    if opts.read_only {
        return Some(guard_fail("read_only", WHY));
    }
    if tailnet_host(request, opts) {
        return Some(guard_fail("tailnet", WHY));
    }
    if !request.remote_addr().is_some_and(|a| a.ip().is_loopback()) {
        return Some(guard_fail("loopback", WHY));
    }
    None
}

/// `GET /api/setup` — setup's checks, detect only
/// ([`crate::setup::board_detect`]): nothing is applied, started or
/// written, provider probes are bounded and never echoed. Each entry is
/// setup's `{check, status, detail, fix}` plus the wizard `group`;
/// `master.providers` carries the master step's provider offers
/// (CAD-448) — its exact start command, never run from the board.
pub(crate) fn setup_get(state_dir: &Path, pm_dir: &Path, port: u16, fresh: bool) -> HttpResp {
    let mut cache = SETUP_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let reuse = cache.as_ref().is_some_and(|(at, _, _)| {
        let age = at.elapsed();
        age < SETUP_MIN_RECHECK || (!fresh && age < SETUP_FRESH_FOR)
    });
    if !reuse {
        SETUP_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let detected = match crate::setup::board_detect(state_dir, pm_dir, port) {
            Ok(d) => d,
            Err(e) => return err_response(500, &e.to_string()),
        };
        let checks: Vec<Value> = detected
            .checks
            .iter()
            .map(|o| {
                let mut v = serde_json::to_value(o).unwrap_or_default();
                v["group"] = json!(crate::setup::check_group(&o.check));
                v
            })
            .collect();
        let checked_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        *cache = Some((
            Instant::now(),
            checked_at,
            json!({
                "checks": checks,
                "master": {"providers": detected.master_providers},
            }),
        ));
    }
    let (at, checked_at, run) = cache.as_ref().expect("filled above");
    let age = at.elapsed();
    json_response(json!({
        "checks": run["checks"],
        "master": run["master"],
        "checked_at": checked_at,
        "detect_only": true,
        // Whether this request ran the checks, how old the run is, and
        // how long until a re-check runs them again.
        "ran_now": !reuse,
        "age_ms": age.as_millis() as u64,
        "recheck_in_ms": SETUP_MIN_RECHECK.saturating_sub(age).as_millis() as u64,
    }))
}

/// The board read model's cost meters (`parses`, `overview_builds`,
/// `request_builds`) for
/// one `(state dir, PM dir)` — what the CAD-325 bench asserts the caches
/// by. Test support; the board serves no route for it.
#[doc(hidden)]
pub fn read_model_stats(state_dir: &Path, pm_dir: &Path) -> Value {
    read_model::get(state_dir, pm_dir).stats()
}

/// One mutex for every write route — the server is thread-per-request
/// since `/api/stream`, and issue file writes must not interleave.
pub(crate) static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn board_boot_agent_uid(state_dir: &Path, injected: Option<u32>) -> Result<Option<u32>> {
    if injected.is_some() {
        return Ok(injected);
    }
    let configured = crate::agent_uid::config::configured_uid(state_dir)?;
    let historical = crate::agent_uid::config::mode_marker_uid(state_dir)?;
    if configured.is_some() || historical.is_some() {
        crate::agent_uid::config::require_private_state_dir(state_dir)?;
    }
    reconcile_board_agent_uid(
        configured,
        historical,
        operator::active_agent_uid(state_dir),
    )
}

pub(crate) fn reconcile_board_agent_uid(
    configured: Option<u32>,
    historical: Option<u32>,
    health: std::result::Result<Option<u32>, String>,
) -> Result<Option<u32>> {
    match health {
        Ok(uid) if configured.is_some() && uid != configured => Err(Error::rejected(
            "Private daemon agent UID differs from configured UID",
        )),
        Ok(uid) if historical.is_some() && uid != historical => Err(Error::rejected(
            "Private daemon agent UID differs from persistent mode marker",
        )),
        Ok(uid) => Ok(uid),
        Err(error) if configured.is_some() || historical.is_some() => Err(Error::rejected(
            format!("Agent UID mode has no private daemon boot pin: {error}"),
        )),
        Err(_) => Ok(None), // Standalone local board, with the split disabled.
    }
}
