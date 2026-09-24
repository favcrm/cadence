//! The Overview slice — `GET /api/overview` and `cadence overview`.
//!
//! One derived answer to "what is waiting on a human or the PM right
//! now, and with which command" plus "is what we merged actually
//! running". Nothing is stored: agents and approvals come from daemon
//! RPCs, review/blocked/project rows from the tracker, PR and CI state
//! from `gh` behind a 60 s cache in the state dir, deploy drift from
//! git walks bounded through `proc::run_bounded`. Every source degrades
//! — an unreachable daemon, a missing tracker, or a failing `gh`
//! narrows the screen instead of failing it.
//!
//! The sources run concurrently and every external probe is bounded
//! (CAD-249): daemon RPCs by [`PROBE_TIMEOUT`] under a pass budget, `gh`
//! by [`GH_TIMEOUT`] plus a caller wait past which the board serves the
//! cache. A probe that misses its bound becomes a `degraded` note.
//! Rows naming the same subject merge into one row carrying `causes`
//! (CAD-252).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::adapter::registry;
use crate::client;
use crate::inbox;
use crate::issue::{self, board, claim, history, project, report};
use crate::proc::run_bounded;

/// Git identity baked in by build.rs — `unknown` when git or a repo
/// was absent at build time, which every consumer reads as "cannot
/// tell", never as zero.
pub const BUILD_COMMIT: &str = env!("CADENCE_BUILD_COMMIT");
pub const BUILD_TIME: &str = env!("CADENCE_BUILD_TIME");
pub const BUILD_REMOTE: &str = env!("CADENCE_BUILD_REMOTE");
pub const BUILD_ROOT: &str = env!("CADENCE_BUILD_ROOT");

/// `gh` results are cached this long in the state dir — the board and
/// the CLI share one cache, and GitHub is the only source allowed to
/// be a network call.
const GH_CACHE_SECS: i64 = 60;
const GH_TIMEOUT: Duration = Duration::from_secs(20);
const GIT_TIMEOUT: Duration = Duration::from_secs(15);
/// `git log` subjects surfaced in the drift tile.
const DRIFT_SUBJECTS: usize = 20;
/// Read bound on each daemon RPC the overview makes (CAD-249).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// The per-agent probe pass starts no probe past this budget; agents
/// it never reached become one `degraded` note.
const PROBE_BUDGET: Duration = Duration::from_secs(4);
/// Concurrent per-agent probes.
const PROBE_WORKERS: usize = 8;
/// The board waits this long on a gh refresh before serving the last
/// cache; the refresh keeps running and lands for the next request.
const GH_BOARD_WAIT: Duration = Duration::from_millis(1500);

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// `YYYY-MM-DDTHH:MM:SSZ` → epoch — the shape `issue::time::iso`
/// writes and the shape `gh` returns for `updatedAt`.
fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // Days-from-civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + sec)
}

/// `(#123)` at the end of a squash-merge subject → the PR number.
fn pr_number(subject: &str) -> Option<u64> {
    let tail = subject.trim_end();
    let inner = tail.strip_suffix(')')?.rsplit("(#").next()?;
    let n: u64 = inner.parse().ok()?;
    Some(n)
}

/// Every rollup entry other than the `qa-verdict` gate must be a pass:
/// CheckRun → COMPLETED + SUCCESS/SKIPPED/NEUTRAL, StatusContext →
/// SUCCESS. An empty rollup means no CI configured — treated green;
/// the human gate is the verdict.
fn checks_green(rollup: &[Value]) -> bool {
    rollup.iter().all(|e| {
        let name = e["context"].as_str().or(e["name"].as_str()).unwrap_or("");
        if name.eq_ignore_ascii_case("qa-verdict") {
            return true;
        }
        if e["__typename"].as_str() == Some("CheckRun") || e["conclusion"].is_string() {
            let done = e["status"].as_str() == Some("COMPLETED") || e["status"].is_null();
            let ok = matches!(
                e["conclusion"].as_str(),
                Some("SUCCESS" | "SKIPPED" | "NEUTRAL")
            );
            return done && ok;
        }
        e["state"].as_str() == Some("SUCCESS")
    })
}

/// The `qa-verdict` commit status on the rollup — "SUCCESS", "FAILURE",
/// "PENDING", or `None` when nobody posted one.
fn verdict_state(rollup: &[Value]) -> Option<String> {
    for e in rollup {
        let name = e["context"].as_str().or(e["name"].as_str()).unwrap_or("");
        if name.eq_ignore_ascii_case("qa-verdict") {
            return e["state"]
                .as_str()
                .or(e["conclusion"].as_str())
                .map(str::to_string);
        }
    }
    None
}

/// Command templates for needs-me rows — every `cadence …` shape the
/// screen can emit. The Cli-parse test in main.rs runs each through
/// `Cli::try_parse_from`, so a row can never carry a command the CLI
/// rejects.
pub fn cmd_agent_unfence(alias: &str) -> String {
    format!("cadence agent unfence {alias}")
}

pub fn cmd_agent_show(alias: &str) -> String {
    format!("cadence agent show {alias}")
}

pub fn cmd_agent_resume(alias: &str) -> String {
    format!("cadence agent resume {alias}")
}

/// The menu-answer command for a pty pane probing `approval_menu` —
/// `<choice>` is the option's printed index on the open menu.
pub fn cmd_agent_answer(alias: &str) -> String {
    format!("cadence agent answer {alias} <choice>")
}

/// The recovery for a silently ended turn: a nudge asking the worker to
/// finish and report. A plain follow-up `send` would queue behind the
/// unreported turn — the actor holds one report-owing turn at a time —
/// while a nudge owns no turn and pastes into the idle pane (CAD-250).
/// `cadence agent attach <alias>` is the manual alternative.
pub fn cmd_send_nudge(alias: &str) -> String {
    format!("cadence send {alias} --nudge --text \"finish and report …\"")
}

pub fn cmd_inbox(alias: &str) -> String {
    format!("cadence inbox {alias}")
}

/// The respond command for a pending request, by request method:
/// provider input requests want an answers file, the approval shapes
/// (provider `*requestApproval`, `session/request_permission`, and
/// brokered `cadence/*`) take a decision, and anything else gets the
/// inspect command — the daemon rejects a respond it cannot map.
pub fn cmd_agent_respond(alias: &str, handle: &str, method: &str) -> String {
    if method == "item/tool/requestUserInput" {
        format!(
            "cadence agent respond {alias} --request {handle} --answers-file answers-{handle}.json"
        )
    } else if method.starts_with("cadence/")
        || method.ends_with("requestApproval")
        || method == "session/request_permission"
    {
        format!("cadence agent respond {alias} --request {handle} --decision accept")
    } else {
        format!("cadence agent requests {alias}")
    }
}

pub fn cmd_issue_show(id: &str) -> String {
    format!("cadence issue show {id}")
}

/// CAD-431: the operator's merge decision on a PASSed ticket.
pub fn cmd_delivery_merge(id: &str) -> String {
    format!("cadence delivery merge {id}")
}

/// CAD-431: decline it instead (the reason is the operator's).
pub fn cmd_delivery_decline(id: &str) -> String {
    format!("cadence delivery decline {id} --reason \"<why>\"")
}

/// CAD-431: re-read the loop's PRs from GitHub (and turn off auto-merge
/// on a moved head).
pub fn cmd_delivery_sync(id: &str) -> String {
    format!("cadence delivery sync {id}")
}

pub fn cmd_issue_set_ready(id: &str) -> String {
    format!("cadence issue set {id} status=ready")
}

pub const CMD_ISSUE_SYNC: &str = "cadence issue sync";
pub const CMD_RESTART_WHEN_IDLE: &str = "cadence daemon restart --when-idle --ui";
/// Deploy-drift remedy (CAD-334): install the newest tested main build.
/// `upgrade` verifies the CI artifact, repoints the CLI, and prints the
/// lease-gated restart ([`CMD_RESTART_WHEN_IDLE`]) as the explicit next
/// step — it never restarts on its own.
pub const CMD_UPGRADE_LATEST_MAIN: &str = "cadence upgrade --latest-main";

/// The monitor daemon deliberately has no external delivery provider in this
/// increment.  The board can still make its durable local inbox useful by
/// projecting the daemon's evidence into the overview and letting an operator
/// acknowledge an alert there.
fn monitor_alert_action(kind: &str, monitor_owner: &str) -> (&'static str, String, &'static str) {
    match kind {
        "approval_menu" => (
            "Review the approval request",
            "operator".to_string(),
            "operator approval required",
        ),
        "attention" => (
            "Resolve the worker attention request",
            "operator".to_string(),
            "operator reconciliation required",
        ),
        "turn_silent_end" => (
            "Reconcile the silent turn outcome",
            "operator".to_string(),
            "operator reconciliation required",
        ),
        "turn_unknown" => (
            "Inspect the uncertain turn and reconcile its worker",
            "operator".to_string(),
            "operator reconciliation required",
        ),
        "draft_pending" => (
            "Route the draft to an independent reviewer",
            "reviewer".to_string(),
            "independent review required",
        ),
        "delivery_parked" => (
            "Inspect the parked delivery before retrying",
            monitor_owner.to_string(),
            "monitor owner may retry; delivery is not automatic",
        ),
        "paste_not_rendered" => (
            "Inspect the PTY render miss and recover the worker",
            monitor_owner.to_string(),
            "monitor owner may recover the worker",
        ),
        "turn_stalled" => (
            "Inspect the stalled turn and reconcile its worker",
            monitor_owner.to_string(),
            "monitor owner may reconcile the worker",
        ),
        "dispatch_blocked" => (
            "Resolve the guarded dispatch prerequisite, then retry the task",
            monitor_owner.to_string(),
            "operator decision required; coordinator cannot bypass the guard",
        ),
        _ => (
            "Inspect the monitor evidence and choose the next owner",
            monitor_owner.to_string(),
            "operator decision required",
        ),
    }
}

fn local_monitor_delivery() -> Value {
    json!({
        "configured": false,
        "state": "ui_local_only",
        "push": false,
        "detail": "Durable alerts are visible and acknowledged in this UI; external push delivery is unconfigured",
    })
}

/// Build the UI-facing monitoring projection from daemon RPC rows.  Keeping
/// this pure makes the state axes testable without opening the live store:
/// `last_success_at` is a completed check, while heartbeat and next-check
/// timestamps remain separate evidence.
fn monitoring_view(
    monitors: Vec<Value>,
    alerts_by_monitor: HashMap<String, Vec<Value>>,
    alert_errors: HashMap<String, String>,
    now: i64,
) -> Value {
    let mut has_degraded = false;
    let mut has_stale = false;
    let mut has_active = false;
    let mut last_success_at: Option<f64> = None;
    let mut last_check_at: Option<f64> = None;
    let mut next_check_at: Option<f64> = None;
    let mut open_alerts = 0i64;
    let mut errors = Vec::new();
    let mut all_alerts = Vec::new();
    let mut monitor_rows = Vec::new();

    for mut monitor in monitors {
        let id = monitor["id"].as_str().unwrap_or_default().to_string();
        let raw_state = monitor["monitoring"].as_str().unwrap_or("off");
        match raw_state {
            "degraded" => has_degraded = true,
            "stale" => has_stale = true,
            "active" => has_active = true,
            _ => {}
        }
        for (field, target) in [
            ("last_success_at", &mut last_success_at),
            ("last_check_at", &mut last_check_at),
        ] {
            if let Some(value) = monitor[field].as_f64() {
                if target.is_none_or(|current| value > current) {
                    *target = Some(value);
                }
            }
        }
        if let Some(value) = monitor["next_check_at"].as_f64() {
            if next_check_at.is_none_or(|current| value < current) {
                next_check_at = Some(value);
            }
        }
        // `active` is a claim about a completed observer pass, not a
        // heartbeat.  Once the recorded schedule is overdue, surface the
        // monitor as stale even if the daemon stopped before it could write
        // a degraded row.  A missing success proof is stale as well.
        let overdue = matches!(raw_state, "active" | "degraded")
            && (monitor["next_check_at"]
                .as_f64()
                .is_none_or(|next| next <= now as f64)
                || (raw_state == "active" && monitor["last_success_at"].is_null()));
        if overdue {
            monitor["stale"] = json!(true);
            monitor["monitoring"] = json!("stale");
            has_stale = true;
            let stale_error = if monitor["last_success_at"].is_null() {
                "active monitor has no successful scan evidence"
            } else {
                "scheduled monitor reconciliation is overdue"
            };
            if monitor["error"].is_null() {
                monitor["error"] = json!(stale_error);
            }
            errors.push(json!({"monitor": id, "error": monitor["error"]}));
        }
        if !overdue {
            if let Some(error) = monitor["error"].as_str().filter(|s| !s.is_empty()) {
                errors.push(json!({"monitor": id, "error": error}));
            }
        }
        if let Some(error) = alert_errors.get(&id) {
            errors.push(json!({"monitor": id, "error": error}));
            monitor["alerts_error"] = json!(error);
        }
        let mut rows = Vec::new();
        for alert in alerts_by_monitor.get(&id).cloned().unwrap_or_default() {
            let kind = alert["kind"].as_str().unwrap_or("alert");
            let (next_action, next_owner, authority) =
                monitor_alert_action(kind, monitor["owner"].as_str().unwrap_or("operator"));
            let created = alert["created"].as_f64().unwrap_or(now as f64);
            let age_secs = ((now as f64 - created).max(0.0)).round() as i64;
            if alert["state"].as_str() == Some("open") {
                open_alerts += 1;
            }
            let row = json!({
                "seq": alert["seq"],
                "monitor": id,
                "project": monitor["project"],
                "monitor_owner": monitor["owner"],
                "task": alert["task"],
                "event_seq": alert["event_seq"],
                "fingerprint": alert["fingerprint"],
                "kind": alert["kind"],
                "payload": alert["payload"],
                "state": alert["state"],
                "attempts": alert["attempts"],
                "last_error": alert["last_error"],
                "created": alert["created"],
                "updated": alert["updated"],
                "age_secs": age_secs,
                "next_action": next_action,
                "next_owner": next_owner,
                "authority": authority,
                "evidence": {
                    "monitor": id,
                    "alert_seq": alert["seq"],
                    "event_seq": alert["event_seq"],
                    "fingerprint": alert["fingerprint"],
                    "payload": alert["payload"],
                },
            });
            rows.push(row.clone());
            all_alerts.push(row);
        }
        monitor["alerts"] = json!(rows);
        monitor_rows.push(monitor);
    }

    let state = if monitor_rows.is_empty() {
        "stopped"
    } else if has_degraded {
        "degraded"
    } else if has_stale {
        "stale"
    } else if has_active {
        "active"
    } else {
        "stopped"
    };
    all_alerts.sort_by_key(|a| a["seq"].as_i64().unwrap_or(0));
    let latest = |value: Option<f64>| value.map(|v| v as i64);
    json!({
        "available": true,
        "state": state,
        "last_success_at": latest(last_success_at),
        "last_check_at": latest(last_check_at),
        "next_check_at": next_check_at.map(|v| v as i64),
        "open_alerts": open_alerts,
        "errors": errors,
        "delivery": local_monitor_delivery(),
        "monitors": monitor_rows,
        "alerts": all_alerts,
    })
}

/// Validate the stable fields emitted by `monitor_list` before projecting any
/// rows. A partially readable response is not a valid empty registration:
/// failing closed keeps the Overview from claiming a healthy subset.
fn validate_monitor_rows(rows: &[Value]) -> Result<(), String> {
    for (index, row) in rows.iter().enumerate() {
        let object = row
            .as_object()
            .ok_or_else(|| format!("monitor_list row {index} is not an object"))?;
        let string_field = |field: &str| {
            object
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("monitor_list row {index} missing {field}"))
        };
        string_field("id")?;
        string_field("project")?;
        string_field("owner")?;
        let state = string_field("monitoring")?;
        if !matches!(state, "active" | "degraded" | "off") {
            return Err(format!(
                "monitor_list row {index} has unsupported monitoring state"
            ));
        }
        if object
            .get("interval_secs")
            .and_then(Value::as_i64)
            .is_none_or(|value| value < 1)
        {
            return Err(format!("monitor_list row {index} missing interval_secs"));
        }
        if object.get("coverage").and_then(Value::as_array).is_none() {
            return Err(format!("monitor_list row {index} missing coverage"));
        }
        let Some(delivery) = object.get("delivery").and_then(Value::as_object) else {
            return Err(format!("monitor_list row {index} missing delivery"));
        };
        if delivery
            .get("configured")
            .and_then(Value::as_bool)
            .is_none()
            || delivery.get("state").and_then(Value::as_str).is_none()
        {
            return Err(format!("monitor_list row {index} has malformed delivery"));
        }
        for field in ["open_alerts", "total_alerts"] {
            if object.get(field).and_then(Value::as_i64).is_none() {
                return Err(format!("monitor_list row {index} missing {field}"));
            }
        }
    }
    Ok(())
}

/// Validate the complete `monitor_alerts` response before exposing any
/// durable evidence to the Overview. An empty alert array is valid; a
/// missing or partially malformed response is unavailable rather than a
/// misleading healthy monitor view.
fn validate_monitor_alert_response<'a>(
    monitor_id: &str,
    response: &'a Value,
) -> Result<&'a [Value], String> {
    let object = response
        .as_object()
        .ok_or_else(|| "monitor_alerts response is not an object".to_string())?;
    let response_monitor = object
        .get("monitor")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if response_monitor != Some(monitor_id) {
        return Err("monitor_alerts response has the wrong monitor".to_string());
    }
    if object
        .get("cursor")
        .and_then(Value::as_i64)
        .is_none_or(|value| value < 0)
    {
        return Err("monitor_alerts response has malformed cursor".to_string());
    }
    let rows = object
        .get("alerts")
        .and_then(Value::as_array)
        .ok_or_else(|| "monitor_alerts response missing alerts".to_string())?;
    for (index, row) in rows.iter().enumerate() {
        let alert = row
            .as_object()
            .ok_or_else(|| format!("monitor_alerts row {index} is not an object"))?;
        let string_field = |field: &str| {
            alert
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("monitor_alerts row {index} missing {field}"))
        };
        if alert
            .get("seq")
            .and_then(Value::as_i64)
            .is_none_or(|value| value < 1)
        {
            return Err(format!("monitor_alerts row {index} missing seq"));
        }
        let row_monitor = string_field("monitor")?;
        if row_monitor != monitor_id {
            return Err(format!(
                "monitor_alerts row {index} belongs to a different monitor"
            ));
        }
        string_field("task")?;
        if alert
            .get("event_seq")
            .and_then(Value::as_i64)
            .is_none_or(|value| value < 1)
        {
            return Err(format!("monitor_alerts row {index} missing event_seq"));
        }
        string_field("fingerprint")?;
        string_field("kind")?;
        if !alert.contains_key("payload") {
            return Err(format!("monitor_alerts row {index} missing payload"));
        }
        if !matches!(
            alert.get("state").and_then(Value::as_str),
            Some("open") | Some("acknowledged") | Some("resolved")
        ) {
            return Err(format!("monitor_alerts row {index} has unsupported state"));
        }
        if alert
            .get("attempts")
            .and_then(Value::as_i64)
            .is_none_or(|value| value < 0)
        {
            return Err(format!("monitor_alerts row {index} missing attempts"));
        }
        if !matches!(
            alert.get("last_error"),
            Some(value) if value.is_null() || value.as_str().is_some()
        ) {
            return Err(format!(
                "monitor_alerts row {index} has malformed last_error"
            ));
        }
        for field in ["created", "updated"] {
            if alert.get(field).and_then(Value::as_f64).is_none() {
                return Err(format!("monitor_alerts row {index} missing {field}"));
            }
        }
    }
    Ok(rows)
}

fn monitoring_unavailable(error: impl Into<String>) -> Value {
    json!({
        "available": false,
        "state": "unavailable",
        "last_success_at": Value::Null,
        "last_check_at": Value::Null,
        "next_check_at": Value::Null,
        "open_alerts": 0,
        "errors": [{"error": error.into()}],
        "delivery": local_monitor_delivery(),
        "monitors": [],
        "alerts": [],
    })
}

/// Read monitor health and durable alerts through the daemon socket.  The
/// board is allowed to show an explicit unavailable state when the daemon is
/// older than the monitor RPC; it must not open the live SQLite store itself.
pub fn monitoring(state_dir: &Path) -> Value {
    let now = now_epoch();
    let list = match client::rpc_timeout(state_dir, "monitor_list", json!({}), PROBE_TIMEOUT) {
        Ok(value) => value,
        Err(error) => {
            return monitoring_unavailable(error.to_string());
        }
    };
    let Some(rows) = list["monitors"].as_array() else {
        return monitoring_unavailable("monitor_list response missing monitors");
    };
    if let Err(error) = validate_monitor_rows(rows) {
        return monitoring_unavailable(error);
    }
    let monitors = rows.to_vec();
    let mut alerts_by_monitor = HashMap::new();
    for monitor in &monitors {
        let id = monitor["id"].as_str().unwrap_or_default();
        match client::rpc_timeout(
            state_dir,
            "monitor_alerts",
            json!({"monitor": id, "open": false, "limit": 100}),
            PROBE_TIMEOUT,
        ) {
            Ok(value) => match validate_monitor_alert_response(id, &value) {
                Ok(rows) => {
                    alerts_by_monitor.insert(id.to_string(), rows.to_vec());
                }
                Err(error) => {
                    return monitoring_unavailable(format!("monitor_alerts '{id}': {error}"));
                }
            },
            Err(error) => {
                return monitoring_unavailable(format!("monitor_alerts '{id}': {error}"));
            }
        }
    }
    monitoring_view(monitors, alerts_by_monitor, HashMap::new(), now)
}

/// A needs-me row before the subject merge and the urgency sort.
struct Item {
    rank: u8,
    age: i64,
    /// `(kind, id)` — rows naming the same agent, issue or PR merge
    /// into one ([`merge_by_subject`]).
    subject: (&'static str, String),
    /// Aliases the row belongs to — `--group` keeps a row when one of
    /// them is a group member.
    agents: Vec<String>,
    /// Who is responsible for acting on the row (CAD-253): an agent
    /// row's upstream PM, an issue or PR row's issue owner, a stale
    /// inbox's owner. `None` — nobody resolvable.
    owner: Option<String>,
    /// Set by [`classify_needs`]; a merged row takes its most urgent cause's.
    audience: Audience,
    /// Epoch seconds when the row's condition began — the unhandled
    /// clock (CAD-253). `None` when the kind has no reliable start: the
    /// row then escalates by owner only, never by age.
    since: Option<i64>,
    json: Value,
}

/// A team-class needs-me row unhandled this long is the operator's
/// (CAD-253, operator decision 2026-09-23).
pub(crate) const ESCALATE_AFTER_SECS: i64 = 60 * 60;

/// Who a needs-me row is for, most urgent first — the derived `Ord`
/// is the merge order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Audience {
    /// "Needs your decision".
    Operator,
    /// A live owner can act on it.
    Team,
    /// Waiting on something outside the fleet (a restart when idle).
    Dependency,
    /// Nothing to decide.
    Info,
}

impl Audience {
    fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Team => "team",
            Self::Dependency => "dependency",
            Self::Info => "info",
        }
    }

    /// The class a kind starts in. Only `team` rows escalate. An
    /// unknown kind is team work, so it can still escalate.
    fn of_kind(kind: &str) -> Self {
        match kind {
            // A fenced agent's exit is `agent unfence` / `message
            // reconcile`, which only the operator may run (CAD-374).
            // CAD-339 Needs-you: a question the master escalated and a
            // plan awaiting approval are the operator's to decide.
            "approval" | "fenced" | "question" | "plan" => Self::Operator,
            // CAD-431: the merge decision, a review that did not
            // converge, one nobody can take, and auto-merge left on a
            // moved head are the operator's.
            "merge_decision"
            | "review_escalated"
            | "review_unstaffed"
            | "auto_merge_on"
            | "delivery_unreadable" => Self::Operator,
            "drift" => Self::Dependency,
            // CAD-439: informs the operator; nothing for the team.
            "inbox_unread" | "tracker_behind" | "master_unconfined" | "master_login" => Self::Info,
            _ => Self::Team,
        }
    }
}

/// Owner liveness from the `agent_list` rows the overview already read.
struct Owners<'a> {
    reachable: bool,
    agents: HashMap<&'a str, &'a Value>,
}

impl<'a> Owners<'a> {
    fn new(reachable: bool, agents: &'a [Value]) -> Self {
        Self {
            reachable,
            agents: agents
                .iter()
                .filter_map(|a| a["alias"].as_str().map(|alias| (alias, a)))
                .collect(),
        }
    }

    /// Why `owner` cannot act on a row — `None` when a live owner can.
    fn cannot_act(&self, owner: Option<&str>) -> Option<String> {
        let Some(owner) = owner.filter(|o| !o.is_empty()) else {
            return Some("no owner".into());
        };
        if owner == inbox::OPERATOR {
            return Some("owner is the operator".into());
        }
        if !self.reachable {
            return Some(format!("owner {owner} unknown — daemon unreachable"));
        }
        let Some(a) = self.agents.get(owner) else {
            return Some(format!("owner {owner} is absent"));
        };
        if a["dead"].as_bool().unwrap_or(false) {
            return Some(format!("owner {owner} is dead"));
        }
        match a["state"].as_str() {
            Some("attention") => return Some(format!("owner {owner} is fenced")),
            Some("stopped") => return Some(format!("owner {owner} is stopped")),
            _ => {}
        }
        // A mailbox owner acts only through whoever drains it.
        if a["inbox_health"]["stale"].as_bool().unwrap_or(false) {
            return Some(format!("owner {owner} has no inbox consumer"));
        }
        None
    }
}

/// Resolve every row's `audience` + `audience_reason` (CAD-253). Kinds
/// already operator-class stay operator; a team row escalates to the
/// operator when its owner cannot act or its condition has stood
/// unhandled past `escalate_after` seconds — measured from `since`,
/// never from the subject's age; a row without `since` escalates by
/// owner only. Runs before [`merge_by_subject`] so each cause is judged
/// on its own owner and clock. The CLI and the board render this
/// field — neither maps kinds to audiences.
fn classify_needs(items: &mut [Item], owners: &Owners, now: i64, escalate_after: i64) {
    for it in items {
        let kind = it.json["kind"].as_str().unwrap_or_default();
        let unhandled = it.since.map(|s| (now - s).max(0));
        let (audience, reason) = match Audience::of_kind(kind) {
            Audience::Operator => (Audience::Operator, Some("operator decision".to_string())),
            Audience::Team => match owners.cannot_act(it.owner.as_deref()) {
                Some(why) => (Audience::Operator, Some(why)),
                None if unhandled.is_some_and(|u| u > escalate_after) => (
                    Audience::Operator,
                    Some(format!("unhandled {}m", unhandled.unwrap_or(0) / 60)),
                ),
                None => (
                    Audience::Team,
                    Some(format!(
                        "owner {} can act",
                        it.owner.as_deref().unwrap_or("")
                    )),
                ),
            },
            other => (other, None),
        };
        it.audience = audience;
        it.json["audience"] = json!(audience.as_str());
        it.json["audience_reason"] = json!(reason);
    }
}

fn item(
    rank: u8,
    kind: &str,
    title: &str,
    age: i64,
    project: &str,
    link: Option<&str>,
    command: &str,
) -> Item {
    let age = age.max(0);
    // Until `about` names it, a row is its own subject.
    let id = format!("{kind}:{title}");
    Item {
        rank,
        age,
        json: json!({
            "kind": kind, "cause": kind, "title": title, "age": age,
            "project": project, "link": link, "command": command,
            "subject": {"kind": "row", "id": id}, "since": null,
        }),
        subject: ("row", id),
        agents: Vec::new(),
        owner: None,
        audience: Audience::of_kind(kind),
        since: None,
    }
}

impl Item {
    /// Name the row's subject: `agent`, `issue`, `pr`, `repo`,
    /// `deploy`, `tracker` or `report`.
    fn about(mut self, kind: &'static str, id: &str) -> Self {
        self.json["subject"] = json!({"kind": kind, "id": id});
        self.subject = (kind, id.to_string());
        self
    }

    fn for_agent(mut self, alias: &str) -> Self {
        if !alias.is_empty() && !self.agents.iter().any(|a| a == alias) {
            self.agents.push(alias.to_string());
        }
        self
    }

    /// Name who must act on the row (CAD-253 escalation reads it).
    fn owned_by(mut self, owner: Option<&str>) -> Self {
        self.owner = owner.filter(|o| !o.is_empty()).map(str::to_string);
        self
    }

    /// When the row's condition began (epoch secs), when known.
    fn since(mut self, since: Option<i64>) -> Self {
        self.set_since(since);
        self
    }

    fn set_since(&mut self, since: Option<i64>) {
        self.since = since;
        self.json["since"] = json!(since);
    }
}

/// CAD-431 Needs-you rows from the daemon's worker-loop record — the
/// only source; a report file cannot raise them. One "merge?" row per
/// PASS that the operator's process saw open and green at the reviewed
/// head, with owner, age, PR link, the verdict's summary and diff stats;
/// one row per review that did not converge or that nobody can take;
/// one per PR whose auto-merge must be turned off.
fn delivery_items(state_dir: &Path, now: i64) -> Vec<Item> {
    let mut out = Vec::new();
    let records = match crate::delivery::load(state_dir) {
        Ok(records) => records,
        Err(e) => {
            // Never a silent empty loop: the operator sees it is broken.
            out.push(
                item(
                    12,
                    "delivery_unreadable",
                    &format!("the review loop's record is unreadable — {e}"),
                    0,
                    "",
                    None,
                    "cadence delivery ls",
                )
                .about("tracker", "delivery.json"),
            );
            return out;
        }
    };
    for rec in records.into_values() {
        let id = rec.issue.as_str();
        let age = now - rec.since;
        let pr = rec.pr.as_deref();
        if rec.disable_auto {
            out.push(
                item(
                    18,
                    "auto_merge_on",
                    &format!("{id}: auto-merge is on for a head nobody approved — turn it off"),
                    age,
                    &rec.project,
                    pr,
                    &cmd_delivery_sync(id),
                )
                .about("issue", id)
                .for_agent(&rec.worker)
                .since(Some(rec.since)),
            );
        }
        let row = match rec.state {
            crate::delivery::State::Passed if rec.merge_ready() => {
                let v = rec
                    .verdict
                    .clone()
                    .unwrap_or_else(|| crate::delivery::VerdictRec {
                        verdict: String::new(),
                        sha: String::new(),
                        reviewer: String::new(),
                        summary: String::new(),
                        report: String::new(),
                        at: rec.since,
                    });
                let o = rec.observed.clone().unwrap_or_default();
                let pr_ref = pr.and_then(crate::delivery::pr_ref);
                let number = pr_ref
                    .as_deref()
                    .map(|r| format!(" {r}"))
                    .unwrap_or_default();
                let mut row = item(
                    22,
                    "merge_decision",
                    &format!(
                        "merge? {id}{number} by {} — PASS by {}: {} (+{} −{}, {} files)",
                        rec.worker, v.reviewer, v.summary, o.additions, o.deletions, o.files
                    ),
                    now - v.at,
                    &rec.project,
                    pr,
                    &cmd_delivery_merge(id),
                )
                .about("issue", id)
                .for_agent(&rec.worker)
                .owned_by(Some(&rec.worker))
                .since(Some(v.at));
                row.json["merge"] = json!({
                    "issue": id, "pr": pr, "pr_ref": pr_ref, "sha": v.sha, "owner": rec.worker,
                    "reviewer": v.reviewer, "verdict_summary": v.summary,
                    "report": v.report, "additions": o.additions,
                    "deletions": o.deletions, "files": o.files,
                    "decline": cmd_delivery_decline(id),
                });
                Some(row)
            }
            crate::delivery::State::Escalated => Some(
                item(
                    24,
                    "review_escalated",
                    &format!(
                        "{id}: {} REVISE verdicts — the review did not converge",
                        rec.revisions
                    ),
                    age,
                    &rec.project,
                    pr,
                    &cmd_issue_show(id),
                )
                .about("issue", id)
                .for_agent(&rec.worker)
                .since(Some(rec.since)),
            ),
            crate::delivery::State::Unstaffed => Some(
                item(
                    26,
                    "review_unstaffed",
                    &format!("{id}: no reviewer is staffed for its review"),
                    age,
                    &rec.project,
                    pr,
                    "cadence agent list",
                )
                .about("issue", id)
                .for_agent(&rec.worker)
                .since(Some(rec.since)),
            ),
            _ => None,
        };
        out.extend(row);
    }
    out
}

/// Urgency order: kind rank ascending, then oldest first inside a kind.
fn sort_needs(needs: &mut [Item]) {
    needs.sort_by(|a, b| a.rank.cmp(&b.rank).then(b.age.cmp(&a.age)));
}

/// One row per subject (CAD-252): rows naming the same agent, issue or
/// PR collapse into the most severe one (lowest rank; ties keep emit
/// order), which lists every cause most severe first under `causes`.
/// `kind`/`cause` stay the primary cause, so a consumer that predates
/// `causes` still reads one sensible row. The CLI and the board both
/// render this merged list — the merge lives here and nowhere else.
fn merge_by_subject(items: Vec<Item>) -> Vec<Item> {
    let mut order: Vec<(&'static str, String)> = Vec::new();
    let mut groups: HashMap<(&'static str, String), Vec<Item>> = HashMap::new();
    for it in items {
        if !groups.contains_key(&it.subject) {
            order.push(it.subject.clone());
        }
        groups.entry(it.subject.clone()).or_default().push(it);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let mut group = groups.remove(&key)?;
            group.sort_by_key(|i| i.rank);
            let causes: Vec<Value> = group
                .iter()
                .map(|i| {
                    json!({
                        "cause": i.json["kind"], "title": i.json["title"],
                        "age": i.age, "command": i.json["command"],
                        "audience": i.json["audience"], "since": i.since,
                    })
                })
                .collect();
            let mut agents: Vec<String> = group.iter().flat_map(|i| i.agents.clone()).collect();
            agents.sort();
            agents.dedup();
            // The row is for whoever its most urgent cause is for — a
            // team primary with an escalated cause is the operator's.
            let urgent = group.iter().min_by_key(|i| i.audience)?;
            let (audience, reason) = (urgent.audience, urgent.json["audience_reason"].clone());
            let mut primary = group.into_iter().next()?;
            primary.json["causes"] = json!(causes);
            primary.agents = agents;
            if primary.audience != audience {
                primary.audience = audience;
                primary.json["audience"] = json!(audience.as_str());
                primary.json["audience_reason"] = reason;
            }
            Some(primary)
        })
        .collect()
}

/// Row scope for `cadence overview --project/--group` (CAD-252).
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// Keep rows attributed to this tracker project key.
    pub project: Option<String>,
    /// Keep rows owned by this group root or one of its members
    /// (`params.upstream == root`), the way `cadence status --group`
    /// resolves a group.
    pub group: Option<String>,
}

/// How one overview build bounds its sources (CAD-249).
#[derive(Clone, Debug)]
pub struct Options {
    pub scope: Scope,
    /// Serve the gh block from the cache only — never fetch, never write.
    pub cache_only: bool,
    /// How long to wait on a gh refresh before serving the last cache.
    pub gh_wait: Duration,
    /// Read bound on each daemon RPC.
    pub probe_timeout: Duration,
    /// The per-agent probe pass starts no probe past this budget.
    pub probe_budget: Duration,
}

impl Options {
    /// One-shot callers (`cadence overview`, `session`): the process
    /// exits after one build, so a background gh refresh would never
    /// land — wait it out (each gh call is bounded by [`GH_TIMEOUT`]).
    pub fn cli() -> Self {
        Self {
            scope: Scope::default(),
            cache_only: false,
            gh_wait: GH_TIMEOUT * 2 + Duration::from_secs(1),
            probe_timeout: PROBE_TIMEOUT,
            probe_budget: PROBE_BUDGET,
        }
    }

    /// The long-lived board server: past [`GH_BOARD_WAIT`] the last
    /// cache is served (`github.state: stale`, with `as_of`) while the
    /// refresh finishes in the background for the next request.
    pub fn board() -> Self {
        Self {
            gh_wait: GH_BOARD_WAIT,
            ..Self::cli()
        }
    }
}

fn git_text(repo: &Path, args: &[String]) -> Result<String, String> {
    let out = run_bounded(
        Command::new("git").arg("-C").arg(repo).args(args),
        GIT_TIMEOUT,
    )
    .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn gh_text(args: &[String]) -> Result<String, String> {
    let out =
        run_bounded(Command::new("gh").args(args), GH_TIMEOUT).map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("gh {}: {}", args.join(" "), err));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The default-branch ref to count drift against: `origin/HEAD`'s
/// target when the symref exists, else origin/main, origin/master,
/// then the local names.
fn default_ref(repo: &Path) -> Result<String, String> {
    if let Ok(sym) = git_text(
        repo,
        &[
            "rev-parse".into(),
            "--abbrev-ref".into(),
            "origin/HEAD".into(),
        ],
    ) {
        let sym = sym.trim();
        if sym.starts_with("origin/") {
            return Ok(sym.to_string());
        }
    }
    for cand in ["origin/main", "origin/master", "main", "master"] {
        if git_text(repo, &["rev-parse".into(), "--verify".into(), cand.into()])
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            return Ok(cand.to_string());
        }
    }
    Err("no default branch ref".to_string())
}

/// Commits on the repo's default branch after `build_commit`: the
/// count plus subjects bounded to [`DRIFT_SUBJECTS`], each with the
/// squash-merged `(#n)` parsed out. `build_commit` "unknown" — or one
/// git cannot place — is `known:false`, "cannot tell", never zero.
fn compute_drift(repo: &Path, build_commit: &str) -> Value {
    let mut base = json!({"known": false, "repo": repo});
    if build_commit == "unknown" || build_commit.is_empty() {
        base["reason"] = json!("build commit unknown — cannot tell");
        return base;
    }
    let dref = match default_ref(repo) {
        Ok(r) => r,
        Err(e) => {
            base["reason"] = json!(format!("cannot tell — {e}"));
            return base;
        }
    };
    let range = format!("{build_commit}..{dref}");
    let count = match git_text(repo, &["rev-list".into(), "--count".into(), range.clone()]) {
        Ok(c) => match c.trim().parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                base["reason"] = json!("cannot tell — unreadable count");
                return base;
            }
        },
        Err(e) => {
            base["reason"] = json!(format!("cannot tell — {e}"));
            return base;
        }
    };
    let subjects = git_text(
        repo,
        &[
            "log".into(),
            format!("-{DRIFT_SUBJECTS}"),
            "--format=%s".into(),
            range,
        ],
    )
    .unwrap_or_default();
    let commits: Vec<Value> = subjects
        .lines()
        .map(|s| json!({"subject": s, "pr": pr_number(s)}))
        .collect();
    json!({
        "known": true, "repo": repo, "ref": dref,
        "build_commit": build_commit,
        "count": count, "commits": commits,
    })
}

/// `gh pr list` plus the default branch's `ci.yml` push runs for one
/// repo slug, fetched concurrently into one cache entry (CAD-267). The
/// legacy commit-status API is not read here: Actions reports check
/// runs, so `commits/HEAD/status` stays `pending, total_count: 0` on a
/// red main. `qa-verdict` still rides the PR rollup. A failing runs
/// fetch never costs the PR rows — it lands as `main_ci.error`.
fn gh_repo(slug: &str) -> Result<Value, String> {
    let (prs, main_ci) = std::thread::scope(|s| {
        let ci = s.spawn(|| gh_main_ci(slug));
        let prs = gh_prs(slug);
        let main_ci = ci
            .join()
            .unwrap_or_else(|_| json!({"error": "ci runs fetch panicked"}));
        (prs, main_ci)
    });
    Ok(json!({"prs": prs?, "main_ci": main_ci}))
}

fn gh_prs(slug: &str) -> Result<Value, String> {
    let prs = gh_text(&[
        "pr".into(),
        "list".into(),
        "--repo".into(),
        slug.into(),
        "--state".into(),
        "open".into(),
        "--limit".into(),
        "50".into(),
        "--json".into(),
        "number,title,url,headRefOid,headRefName,updatedAt,statusCheckRollup".into(),
    ])?;
    serde_json::from_str::<Value>(&prs).map_err(|e| format!("gh pr list: unreadable ({e})"))
}

/// The run fields [`classify_main_ci`] and the rows read — the rest of
/// the API object is dropped before it reaches the cache.
const RUN_FIELDS: [&str; 9] = [
    "id",
    "head_sha",
    "status",
    "conclusion",
    "event",
    "path",
    "head_branch",
    "html_url",
    "created_at",
];

/// `{branch, runs}` for the repo's default branch: the newest
/// [`CI_RUNS_PAGE`] `ci.yml` push runs. A repo without a `ci.yml`
/// workflow is `{absent: true}` — no CI to read, not an error; any
/// other failure is `{error}`.
fn gh_main_ci(slug: &str) -> Value {
    let branch = match gh_text(&["api".into(), format!("repos/{slug}")])
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| format!("unreadable ({e})")))
    {
        Ok(repo) => match repo["default_branch"].as_str() {
            Some(b) if is_plain_ref(b) => b.to_string(),
            _ => return json!({"error": format!("repos/{slug}: no readable default_branch")}),
        },
        Err(e) => return json!({"error": e}),
    };
    let path = format!(
        "repos/{slug}/actions/workflows/{CI_WORKFLOW}/runs?branch={branch}&event=push&per_page={CI_RUNS_PAGE}"
    );
    let body = match gh_text(&["api".into(), path]) {
        Ok(t) => t,
        Err(e) if e.contains("HTTP 404") => return json!({"branch": branch, "absent": true}),
        Err(e) => return json!({"branch": branch, "error": e}),
    };
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return json!({"branch": branch, "error": format!("ci runs: unreadable ({e})")}),
    };
    let Some(runs) = parsed["workflow_runs"].as_array() else {
        return json!({"branch": branch, "error": "ci runs: no workflow_runs array"});
    };
    let runs: Vec<Value> = runs
        .iter()
        .map(|r| {
            RUN_FIELDS
                .iter()
                .map(|f| (f.to_string(), r[*f].clone()))
                .collect::<serde_json::Map<_, _>>()
                .into()
        })
        .collect();
    json!({"branch": branch, "runs": runs})
}

/// A branch name safe to put in a query string and a git revision
/// unquoted.
fn is_plain_ref(b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with('-')
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
}

// ---------- default-branch CI (CAD-267) ----------

/// The workflow whose push runs are the default branch's CI verdict.
/// Runs of any other workflow (Handover, …) never count.
const CI_WORKFLOW: &str = "ci.yml";
/// Push runs fetched per refresh — one per pushed SHA under the
/// `queue: max` policy, so a page covers more SHAs than we classify.
const CI_RUNS_PAGE: usize = 30;
/// First-parent SHAs of the default branch the overview classifies.
const MAIN_CI_SHAS: usize = 20;
/// First-parent SHAs read from the local clone to place each run.
const MAIN_CI_LOG: usize = 60;

/// One default-branch SHA's own `ci.yml` verdict. Only a SHA's own
/// successful push run makes it `Passed` — never absence, another
/// workflow, or a later SHA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CiState {
    Passed,
    /// `failure`, `timed_out`, `startup_failure`.
    Failed,
    /// The run exists and has not completed.
    Pending,
    /// The run completed without a verdict: `cancelled`, or `skipped`,
    /// `neutral`, `stale`, `action_required` (see `conclusion`).
    Cancelled,
    /// No `ci.yml` push run for this SHA at all.
    Missing,
}

impl CiState {
    fn as_str(self) -> &'static str {
        match self {
            CiState::Passed => "passed",
            CiState::Failed => "failed",
            CiState::Pending => "pending",
            CiState::Cancelled => "cancelled",
            CiState::Missing => "missing",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShaCi {
    pub sha: String,
    pub state: CiState,
    /// Cancelled/missing only: the nearest later SHA whose own run
    /// passed. The SHA itself stays unverified — covered, not passed.
    pub covered_by: Option<String>,
    /// The run that decided `state` — `None` when missing.
    pub run_id: Option<u64>,
    pub run_url: Option<String>,
    pub conclusion: Option<String>,
    pub created_at: Option<String>,
}

impl ShaCi {
    fn to_json(&self) -> Value {
        json!({
            "sha": self.sha,
            "state": self.state.as_str(),
            "covered_by": self.covered_by,
            "run_id": self.run_id,
            "run_url": self.run_url,
            "conclusion": self.conclusion,
            "created_at": self.created_at,
        })
    }
}

fn is_ci_run(r: &Value) -> bool {
    let path = r["path"].as_str().unwrap_or_default();
    let file = path.split('@').next().unwrap_or_default();
    r["event"].as_str() == Some("push")
        && (file == CI_WORKFLOW || file.ends_with(&format!("/{CI_WORKFLOW}")))
}

fn run_state(r: &Value) -> CiState {
    if r["status"].as_str() != Some("completed") {
        return CiState::Pending;
    }
    match r["conclusion"].as_str() {
        Some("success") => CiState::Passed,
        Some("failure" | "timed_out" | "startup_failure") => CiState::Failed,
        _ => CiState::Cancelled,
    }
}

/// Classify the default branch's recent SHAs by their own `ci.yml`
/// push run (CAD-267). Pure — fixtures test it.
///
/// `first_parent` is `git log --first-parent` of the default branch,
/// newest first. A run SHA it does not contain but whose run is newer
/// than every listed SHA's run was pushed after the clone's last fetch:
/// it leads the list, newest run first (with no clone at all, the runs
/// alone give the order). Older unlisted run SHAs are not on the
/// branch's first-parent history and are ignored. Per SHA the newest
/// run (highest id) decides; returns at most [`MAIN_CI_SHAS`], newest
/// first.
pub(crate) fn classify_main_ci(runs: &[Value], first_parent: &[String]) -> Vec<ShaCi> {
    let mut own: HashMap<&str, &Value> = HashMap::new();
    for r in runs.iter().filter(|r| is_ci_run(r)) {
        let Some(sha) = r["head_sha"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let id = r["id"].as_u64().unwrap_or(0);
        if own
            .get(sha)
            .is_none_or(|prev| prev["id"].as_u64().unwrap_or(0) < id)
        {
            own.insert(sha, r);
        }
    }
    let created = |sha: &str| {
        own.get(sha)
            .and_then(|r| r["created_at"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    let listed: std::collections::HashSet<&str> = first_parent.iter().map(String::as_str).collect();
    let newest_listed = first_parent
        .iter()
        .map(|s| created(s))
        .max()
        .unwrap_or_default();
    let mut ahead: Vec<&str> = own
        .keys()
        .copied()
        .filter(|s| !listed.contains(s) && created(s) > newest_listed)
        .collect();
    ahead.sort_by_key(|s| std::cmp::Reverse((created(s), own[s]["id"].as_u64().unwrap_or(0))));
    let mut out: Vec<ShaCi> = ahead
        .into_iter()
        .chain(first_parent.iter().map(String::as_str))
        .map(|sha| {
            let run = own.get(sha);
            ShaCi {
                sha: sha.to_string(),
                state: run.map_or(CiState::Missing, |r| run_state(r)),
                covered_by: None,
                run_id: run.and_then(|r| r["id"].as_u64()),
                run_url: run.and_then(|r| r["html_url"].as_str().map(str::to_string)),
                conclusion: run.and_then(|r| r["conclusion"].as_str().map(str::to_string)),
                created_at: run.and_then(|r| r["created_at"].as_str().map(str::to_string)),
            }
        })
        .collect();
    // Newest first: the last pass seen before index i is the nearest
    // later SHA whose own run passed.
    let mut nearest_pass: Option<String> = None;
    for s in &mut out {
        if matches!(s.state, CiState::Cancelled | CiState::Missing) {
            s.covered_by = nearest_pass.clone();
        }
        if s.state == CiState::Passed {
            nearest_pass = Some(s.sha.clone());
        }
    }
    out.truncate(MAIN_CI_SHAS);
    out
}

/// What the classification alerts on. `red`: the newest SHA with a
/// verdict (passed or failed) failed — pending and cancelled SHAs
/// carry no verdict, so they neither raise nor clear it.
/// `unverified`: cancelled/missing SHAs with no covering later pass,
/// newest first — they clear the moment a later SHA passes, while the
/// SHAs keep their own label.
pub(crate) struct MainCiAlerts<'a> {
    pub red: Option<&'a ShaCi>,
    pub unverified: Vec<&'a ShaCi>,
}

pub(crate) fn main_ci_alerts(shas: &[ShaCi]) -> MainCiAlerts<'_> {
    let red = shas
        .iter()
        .find(|s| matches!(s.state, CiState::Passed | CiState::Failed))
        .filter(|s| s.state == CiState::Failed);
    let unverified = shas
        .iter()
        .filter(|s| matches!(s.state, CiState::Cancelled | CiState::Missing))
        .filter(|s| s.covered_by.is_none())
        .collect();
    MainCiAlerts { red, unverified }
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// `git log --first-parent` of `branch` in the local clone, newest
/// first — `origin/<branch>` when the clone tracks it, else the local
/// branch. Local refs only, never a fetch.
fn first_parent_log(repo: &Path, branch: &str) -> Result<Vec<String>, String> {
    let rev = [format!("origin/{branch}"), branch.to_string()]
        .into_iter()
        .find(|r| {
            git_text(
                repo,
                &[
                    "rev-parse".into(),
                    "--verify".into(),
                    "-q".into(),
                    r.clone(),
                ],
            )
            .is_ok()
        })
        .ok_or_else(|| format!("no {branch} ref in {}", repo.display()))?;
    let text = git_text(
        repo,
        &[
            "log".into(),
            "--first-parent".into(),
            format!("-{MAIN_CI_LOG}"),
            "--format=%H".into(),
            rev,
        ],
    )?;
    Ok(text.lines().map(str::to_string).collect())
}

/// The `main_ci` block for one slug plus its needs-me rows.
fn main_ci_view(
    slug: &str,
    project: &str,
    data: &Value,
    clone: Option<&Path>,
    now: i64,
) -> (Value, Vec<Item>) {
    let block = &data["main_ci"];
    // A cache body written before CAD-267 has no block — nothing to say.
    if block.is_null() {
        return (Value::Null, Vec::new());
    }
    let Some(branch) = block["branch"].as_str() else {
        return (
            json!({"slug": slug, "project": project, "error": block["error"]}),
            Vec::new(),
        );
    };
    if block["absent"].as_bool() == Some(true) {
        return (Value::Null, Vec::new());
    }
    let Some(runs) = block["runs"].as_array() else {
        return (
            json!({"slug": slug, "project": project, "branch": branch, "error": block["error"]}),
            Vec::new(),
        );
    };
    let (first_parent, log_error) = match clone.map(|c| first_parent_log(c, branch)) {
        Some(Ok(log)) => (log, None),
        Some(Err(e)) => (Vec::new(), Some(e)),
        None => (Vec::new(), Some("no local clone declared".to_string())),
    };
    let shas = classify_main_ci(runs, &first_parent);
    let alerts = main_ci_alerts(&shas);
    let subject = format!("{slug}@{branch}");
    let run_at = |s: &ShaCi| s.created_at.as_deref().and_then(parse_iso);
    let run_age = |s: &ShaCi| run_at(s).map(|t| now - t).unwrap_or(0);
    let mut rows = Vec::new();
    if let Some(s) = alerts.red {
        let command = match s.run_id {
            Some(id) => format!("gh run view {id} --repo {slug}"),
            None => format!("gh run list --repo {slug} --workflow {CI_WORKFLOW} --branch {branch}"),
        };
        rows.push(
            item(
                90,
                "ci_red",
                &format!("{branch} CI failed at {} — {slug}", short(&s.sha)),
                run_age(s),
                project,
                s.run_url.as_deref(),
                &command,
            )
            .about("ci", &subject)
            .since(run_at(s)),
        );
    }
    if let Some(newest) = alerts.unverified.first() {
        let n = alerts.unverified.len();
        let what = format!("{} {}", short(&newest.sha), newest.state.as_str());
        let desc = if n == 1 {
            what
        } else {
            format!("{n} SHAs, newest {what}")
        };
        // Re-running the newest cancelled run covers every older one
        // once it passes; a missing run has nothing to re-run.
        let command = match (newest.state, newest.run_id) {
            (CiState::Cancelled, Some(id)) => format!("gh run rerun {id} --repo {slug}"),
            _ => format!("gh run list --repo {slug} --workflow {CI_WORKFLOW} --branch {branch}"),
        };
        rows.push(
            item(
                92,
                "ci_unverified",
                &format!("{branch} CI unverified: {desc}, no later SHA passed yet — {slug}"),
                run_age(newest),
                project,
                newest.run_url.as_deref(),
                &command,
            )
            .about("ci", &subject)
            .since(run_at(newest)),
        );
    }
    let view = json!({
        "slug": slug,
        "project": project,
        "branch": branch,
        "workflow": CI_WORKFLOW,
        "order": if first_parent.is_empty() { "runs" } else { "first_parent" },
        "log_error": log_error,
        "shas": shas.iter().map(ShaCi::to_json).collect::<Vec<_>>(),
    });
    (view, rows)
}

fn cache_file(state_dir: &Path) -> PathBuf {
    state_dir.join("overview-gh.json")
}

/// The cache body on disk: the slug set the rows were fetched for
/// plus the repo payloads. A body only serves a request for the same
/// slug set — a tracker with no GitHub remotes must not blank the
/// board's rows, and a different tracker must not inherit them.
#[derive(Clone)]
struct GhCache {
    at: i64,
    slugs: Vec<String>,
    repos: HashMap<String, Value>,
}

fn read_cache(file: &Path) -> Option<GhCache> {
    let text = std::fs::read_to_string(file).ok()?;
    let cached: Value = serde_json::from_str(&text).ok()?;
    let at = cached["at"].as_i64()?;
    let slugs = cached["slugs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let repos = cached["repos"]
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    Some(GhCache { at, slugs, repos })
}

/// Temp-write then rename — a crashed reader never sees half a body.
fn write_cache(file: &Path, slugs: &[String], repos: &HashMap<String, Value>, at: i64) {
    let tmp = file.with_extension("tmp");
    let body = serde_json::to_string(&json!({
        "at": at, "slugs": slugs, "repos": repos,
    }))
    .unwrap_or_default();
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
}

/// One repo's `gh` read — [`gh_repo`] in production, a stub in tests.
type GhFetch = fn(&str) -> Result<Value, String>;

/// gh refreshes in flight in this process, by cache file: while one
/// runs, other requests serve the cache instead of starting another.
static GH_REFRESHING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The GitHub block, 60 s-cached under the state dir, waiting as long
/// as the one-shot CLI needs.
fn github(state_dir: &Path, slugs: &[String]) -> (HashMap<String, Value>, Value) {
    github_bounded(state_dir, slugs, Options::cli().gh_wait, gh_repo)
}

/// The GitHub block with a bounded wait (CAD-249). Returns the repos
/// map plus `{state: ok|cached|stale|unavailable, as_of, error?}` —
/// `as_of` is when the rows were fetched. A fresh cache answers at
/// once; otherwise a refresh starts (every slug concurrently) and the
/// caller waits at most `wait` for it. Past that, the last good body
/// for this slug set is served as `stale` while the refresh finishes
/// in the background and lands in the cache for the next request.
fn github_bounded(
    state_dir: &Path,
    slugs: &[String],
    wait: Duration,
    fetch: GhFetch,
) -> (HashMap<String, Value>, Value) {
    let file = cache_file(state_dir);
    let now = now_epoch();
    let cached = read_cache(&file);
    if let Some(c) = &cached {
        if now - c.at < GH_CACHE_SECS && c.slugs == slugs {
            return (
                c.repos.clone(),
                json!({"state": "cached", "at": c.at, "as_of": c.at}),
            );
        }
    }
    if slugs.is_empty() {
        // Nothing to fetch — and nothing to write: an empty slug set
        // must never stamp over a good cache.
        return (HashMap::new(), json!({"state": "ok", "as_of": now}));
    }
    let claimed = {
        let mut running = GH_REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains(&file) {
            false
        } else {
            running.push(file.clone());
            true
        }
    };
    if claimed {
        let (tx, rx) = mpsc::channel();
        let (dir, want, prior) = (state_dir.to_path_buf(), slugs.to_vec(), cached.clone());
        std::thread::spawn(move || {
            let out = refresh_github(&dir, &want, prior, fetch);
            GH_REFRESHING
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|f| f != &file);
            let _ = tx.send(out);
        });
        if let Ok(out) = rx.recv_timeout(wait) {
            return out;
        }
    }
    stale_github(
        cached,
        slugs,
        Some(format!(
            "github refresh still running after {:.1}s — serving the last cache",
            wait.as_secs_f64()
        )),
    )
}

/// The last good rows for this slug set, untimed — better stale rows
/// than blank ones. `unavailable` when the cache covers none of them.
fn stale_github(
    cached: Option<GhCache>,
    slugs: &[String],
    error: Option<String>,
) -> (HashMap<String, Value>, Value) {
    let at = cached.as_ref().map(|c| c.at);
    let stale: HashMap<String, Value> = cached
        .map(|c| {
            c.repos
                .into_iter()
                .filter(|(k, _)| slugs.contains(k))
                .collect()
        })
        .unwrap_or_default();
    if stale.is_empty() {
        return (
            stale,
            json!({"state": "unavailable", "error": error, "as_of": null}),
        );
    }
    (
        stale,
        json!({"state": "stale", "error": error, "as_of": at}),
    )
}

/// Fetch every slug concurrently (each `gh` call bounded by
/// [`GH_TIMEOUT`]), fill failed slugs from the cache, and write the
/// cache when anything came back.
fn refresh_github(
    state_dir: &Path,
    slugs: &[String],
    cached: Option<GhCache>,
    fetch: GhFetch,
) -> (HashMap<String, Value>, Value) {
    let results: Vec<Result<Value, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = slugs
            .iter()
            .map(|slug| s.spawn(move || fetch(slug)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("gh fetch panicked".to_string()))
            })
            .collect()
    });
    let now = now_epoch();
    let mut repos = HashMap::new();
    let mut first_err = None;
    for (slug, result) in slugs.iter().zip(results) {
        match result {
            Ok(v) => {
                repos.insert(slug.clone(), v);
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if repos.is_empty() {
        // Every call failed: keep the last good rows for this slug set.
        return stale_github(cached, slugs, first_err);
    }
    // Partial failure: stale rows fill the missing slugs when the
    // cache covered them, so one flaky repo can't blank its PRs.
    if let Some(c) = cached {
        for slug in slugs {
            if !repos.contains_key(slug) {
                if let Some(v) = c.repos.get(slug) {
                    repos.insert(slug.clone(), v.clone());
                }
            }
        }
    }
    let _ = std::fs::create_dir_all(state_dir);
    write_cache(&cache_file(state_dir), slugs, &repos, now);
    (
        repos,
        json!({"state": "ok", "error": first_err, "as_of": now}),
    )
}

/// The tracker project matching the repo this binary was built from:
/// remote first (normalised both sides), then declared path against
/// the build checkout. Returns the project key and the local clone to
/// walk — the declared `path`, else the build root itself.
fn build_repo_match(projects: &[project::Project]) -> Option<(String, PathBuf)> {
    let build_remote = if BUILD_REMOTE == "unknown" {
        None
    } else {
        Some(project::normalize_remote(BUILD_REMOTE))
    };
    let build_root = if BUILD_ROOT == "unknown" {
        None
    } else {
        Some(
            PathBuf::from(BUILD_ROOT)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(BUILD_ROOT)),
        )
    };
    for p in projects {
        for r in &p.repos {
            let remote_hit = match (&build_remote, &r.remote) {
                (Some(want), Some(have)) => project::normalize_remote(have) == *want,
                _ => false,
            };
            let declared = r.path.as_deref().map(project::expand_home);
            let path_hit = match (&build_root, &declared) {
                (Some(want), Some(have)) => {
                    have.canonicalize().unwrap_or_else(|_| have.clone()) == *want
                }
                _ => false,
            };
            if remote_hit || path_hit {
                let repo = declared
                    .clone()
                    .or_else(|| build_root.clone())
                    .unwrap_or_else(|| PathBuf::from("."));
                return Some((p.key.clone(), repo));
            }
        }
    }
    None
}

/// The whole screen. `pm_dir` names the tracker dir (it may not exist
/// — that just empties the tracker sections); `state_dir` names the
/// runtime dir (daemon socket + the gh cache).
pub fn overview(state_dir: &Path, pm_dir: &Path) -> Value {
    unscoped(overview_with(state_dir, pm_dir, &Options::cli()))
}

/// The overview with the gh block served from the cache only — a dry
/// run must write nothing, cache included, so it never fetches.
pub(crate) fn overview_cached(state_dir: &Path, pm_dir: &Path) -> Value {
    let opts = Options {
        cache_only: true,
        ..Options::cli()
    };
    unscoped(overview_with(state_dir, pm_dir, &opts))
}

/// `GET /api/overview` — bounded gh wait, background refresh.
pub fn overview_board(state_dir: &Path, pm_dir: &Path) -> Value {
    unscoped(overview_with(state_dir, pm_dir, &Options::board()))
}

/// Only a scope can fail a build, and these callers pass none.
fn unscoped(view: Result<Value, String>) -> Value {
    view.unwrap_or_else(|e| json!({"error": e}))
}

/// A `degraded` note: one source answered late or not at all, and the
/// screen narrowed instead of failing.
fn degraded(source: &str, subject: &str, detail: impl Into<String>) -> Value {
    json!({"source": source, "subject": subject, "detail": detail.into()})
}

/// What one agent's probes returned. A mailbox (no actor) needs none —
/// its `agent_list` row carries the backlog and health.
#[derive(Clone, Default)]
struct AgentProbe {
    show: Option<Value>,
    requests: Vec<Value>,
    /// A running/submitted turn, a busy pane, or a probe that could not
    /// tell — any of them holds the drift restart back.
    holds_drift: bool,
    /// The daemon predates `agent_probe`.
    probe_unknown: bool,
    /// The probe budget ran out before this agent was reached.
    unprobed: bool,
    degraded: Vec<Value>,
}

/// One daemon RPC under the overview's bounds: the per-call timeout,
/// clipped to what is left of the pass's deadline.
fn bounded_rpc(
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
    deadline: Instant,
) -> Result<Value, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left < Duration::from_millis(50) {
        return Err("overview probe budget spent".to_string());
    }
    let bound = left.min(timeout);
    client::rpc_timeout(state_dir, method, params, bound).map_err(|e| {
        let text = e.to_string();
        // A read timeout surfaces as EAGAIN/"timed out" — name the bound.
        if text.contains("os error 11") || text.contains("timed out") {
            format!("no answer within {}ms", bound.as_millis())
        } else {
            text
        }
    })
}

/// Show, pending requests and (for an idle pty pane) the pane probe of
/// one agent, each bounded.
fn probe_agent(state_dir: &Path, a: &Value, timeout: Duration, deadline: Instant) -> AgentProbe {
    let alias = a["alias"].as_str().unwrap_or_default();
    let provider = a["provider"].as_str().unwrap_or_default();
    let kind = a["endpoint_kind"].as_str().unwrap_or_default();
    let mut p = AgentProbe::default();
    // A mailbox has no pane and no requests: its `agent_list` row
    // carries the backlog. Only rows that predate the `inbox` block
    // (an older daemon) need the show read for the queued count.
    let mailbox = !registry::has_actor(provider, kind);
    if mailbox && a["inbox"]["queued"].is_i64() {
        return p;
    }
    if deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(50) {
        p.unprobed = true;
        // A mailbox never holds a restart back.
        p.holds_drift = !mailbox;
        return p;
    }
    let rpc = |method: &str| {
        bounded_rpc(
            state_dir,
            method,
            json!({"alias": alias}),
            timeout,
            deadline,
        )
    };
    match rpc("agent_show") {
        Ok(v) => p.show = Some(v),
        Err(e) => p.degraded.push(degraded("agent_show", alias, e)),
    }
    if mailbox {
        return p;
    }
    match rpc("agent_requests") {
        Ok(v) => p.requests = v["requests"].as_array().cloned().unwrap_or_default(),
        Err(e) if e.contains("Unknown method") => {}
        Err(e) => p.degraded.push(degraded("agent_requests", alias, e)),
    }
    // Drift is only actionable when everything is idle: any running or
    // submitted message, or any busy pty pane, holds it back — and so
    // does a show that never answered.
    let busy_turn = p.show.as_ref().is_none_or(|show| {
        show["messages"].as_array().is_some_and(|ms| {
            ms.iter().any(|m| {
                matches!(
                    m["state"].as_str().unwrap_or_default(),
                    "running" | "submitted"
                )
            })
        })
    });
    if busy_turn {
        p.holds_drift = true;
    } else if kind == "pty" && a["endpoint"].is_string() {
        match rpc("agent_probe") {
            Ok(v) if v["idle"].as_bool().unwrap_or(false) => {}
            Ok(_) => p.holds_drift = true,
            // "Unknown method" — a daemon that predates the RPC; any
            // other failure reads as busy, conservatively.
            Err(e) if e.contains("Unknown method") => p.probe_unknown = true,
            Err(e) => {
                p.holds_drift = true;
                p.degraded.push(degraded("agent_probe", alias, e));
            }
        }
    }
    p
}

/// Probe every agent on [`PROBE_WORKERS`] threads — the daemon answers
/// each connection on its own thread, so the pass costs the slowest
/// probes, not their sum. No probe starts past `opts.probe_budget`.
fn probe_agents(state_dir: &Path, agents: &[Value], opts: &Options) -> Vec<AgentProbe> {
    let deadline = Instant::now() + opts.probe_budget;
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<AgentProbe>>> = agents.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..PROBE_WORKERS.min(agents.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(a) = agents.get(i) else {
                    break;
                };
                let p = probe_agent(state_dir, a, opts.probe_timeout, deadline);
                *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or_default()
        })
        .collect()
}

/// The daemon's half of the screen: reachability, build identity, the
/// agent rows and their probes.
#[derive(Clone)]
struct DaemonView {
    reachable: bool,
    info: Option<Value>,
    agents: Vec<Value>,
    probes: Vec<AgentProbe>,
    degraded: Vec<Value>,
}

fn daemon_view(state_dir: &Path, opts: &Options) -> DaemonView {
    let t = opts.probe_timeout;
    let mut view = DaemonView {
        // Reachability comes from `health` — a pre-daemon_info daemon
        // answers it, so an old build never reads as "unreachable"
        // while `agent list` works. `daemon_info` only carries the id.
        reachable: client::rpc_timeout(state_dir, "health", json!({}), t).is_ok(),
        info: None,
        agents: Vec::new(),
        probes: Vec::new(),
        degraded: Vec::new(),
    };
    if !view.reachable {
        return view;
    }
    view.info = client::rpc_timeout(state_dir, "daemon_info", json!({}), t).ok();
    match client::rpc_timeout(state_dir, "agent_list", json!({}), t) {
        Ok(v) => view.agents = v["agents"].as_array().cloned().unwrap_or_default(),
        Err(e) => view.degraded.push(degraded(
            "agent_list",
            "",
            format!("agent rows missing — {e}"),
        )),
    }
    view.probes = probe_agents(state_dir, &view.agents, opts);
    let unprobed: Vec<&str> = view
        .agents
        .iter()
        .zip(&view.probes)
        .filter(|(_, p)| p.unprobed)
        .filter_map(|(a, _)| a["alias"].as_str())
        .collect();
    if !unprobed.is_empty() {
        view.degraded.push(degraded(
            "agent_probe_budget",
            "",
            format!(
                "{} agent(s) not probed within {:.1}s: {}",
                unprobed.len(),
                opts.probe_budget.as_secs_f64(),
                unprobed.join(", ")
            ),
        ));
    }
    for p in &mut view.probes {
        view.degraded.append(&mut p.degraded);
    }
    view
}

/// The daemon-side sources of one build — the agent probes and the
/// monitoring block. The board's read model (CAD-325) keeps the last
/// pass and rebuilds the screen from it when only the tracker moved.
#[derive(Clone)]
pub struct DaemonSources {
    daemon: DaemonView,
    monitoring: Value,
}

/// Probe the daemon and read the monitors, concurrently.
pub fn daemon_sources(state_dir: &Path, opts: &Options) -> DaemonSources {
    std::thread::scope(|s| {
        let daemon = s.spawn(|| daemon_view(state_dir, opts));
        let monitoring = s.spawn(|| monitoring(state_dir));
        DaemonSources {
            daemon: daemon.join().expect("overview daemon probe panicked"),
            monitoring: monitoring
                .join()
                .expect("overview monitoring read panicked"),
        }
    })
}

/// Git-derived status and claim times kept across builds (CAD-325), per
/// `(kind, issue id)` with the key they were read under — the tracker's
/// `HEAD` plus the `issue.md` rev. The answer is a `git log`, so it moves
/// with history as well as the file: an edit, a commit, a reset or a pull
/// each re-read it, and nothing else does. Only found times are kept — a
/// `None` may be a spent budget or a timed-out `git`.
pub type ClockCache = Mutex<HashMap<(&'static str, String), (String, i64)>>;

/// The clock cache as one build uses it: the cache and the tracker `HEAD`
/// this build read (`git rev-parse HEAD`, once per build).
#[derive(Clone, Copy)]
struct SharedClocks<'a> {
    cache: &'a ClockCache,
    head: &'a str,
}

impl SharedClocks<'_> {
    /// The cache key for `v` now, `None` when `issue.md` is unreadable.
    fn key(&self, v: &board::View) -> Option<String> {
        let rev = issue::write::issue_rev(&v.issue.dir).ok()?;
        Some(format!("{} {rev}", self.head))
    }

    fn get(&self, kind: &'static str, id: &str, key: &str) -> Option<i64> {
        let map = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&(kind, id.to_string()))
            .filter(|(k, _)| k == key)
            .map(|(_, at)| *at)
    }

    fn put(&self, kind: &'static str, id: &str, key: String, at: Option<i64>) {
        if let Some(at) = at {
            self.cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((kind, id.to_string()), (key, at));
        }
    }
}

/// What a long-lived caller hands a build instead of re-reading it: the
/// indexed tracker's views, a recent daemon pass and the clock cache.
pub struct Reuse<'a> {
    pub views: &'a [board::View],
    pub sources: &'a DaemonSources,
    pub clocks: &'a ClockCache,
}

/// Total wall time the tracker status clocks may spend per build; rows
/// past it get no clock (owner-only escalation) and one `degraded` note.
const STATUS_CLOCK_BUDGET: Duration = Duration::from_secs(3);

/// When each issue entered its current effective status (CAD-253), read
/// once per issue per build under one time budget. A `file` status is
/// the tracker's last `status:` change ([`history::status_changed_at`]);
/// a `notes` status is the deriving note's time; a `rollup` or `job`
/// status has no single change to point at, so no clock.
struct StatusClock<'a> {
    pm_dir: &'a Path,
    deadline: Instant,
    cache: HashMap<String, Option<i64>>,
    /// The read model's cache across builds, when there is one.
    shared: Option<SharedClocks<'a>>,
    /// Issues left without a clock because the budget ran out.
    skipped: usize,
}

impl<'a> StatusClock<'a> {
    fn new(pm_dir: &'a Path, budget: Duration, shared: Option<SharedClocks<'a>>) -> Self {
        Self {
            pm_dir,
            deadline: Instant::now() + budget,
            cache: HashMap::new(),
            shared,
            skipped: 0,
        }
    }

    fn since(&mut self, v: &board::View) -> Option<i64> {
        let id = &v.issue.front.id;
        if let Some(hit) = self.cache.get(id) {
            return *hit;
        }
        let at = match v.status_source {
            "file" => {
                let shared = self.shared.and_then(|c| Some((c, c.key(v)?)));
                if let Some(hit) = shared.as_ref().and_then(|(c, k)| c.get("status", id, k)) {
                    self.cache.insert(id.clone(), Some(hit));
                    return Some(hit);
                }
                let left = self.deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    self.skipped += 1;
                    None
                } else {
                    let at = history::status_changed_at(
                        self.pm_dir,
                        &v.issue.project,
                        id,
                        left.min(GIT_TIMEOUT),
                    );
                    if let Some((c, k)) = shared {
                        c.put("status", id, k, at);
                    }
                    at
                }
            }
            "notes" => v.chain.last().and_then(|n| parse_iso(&n.at)),
            _ => None,
        };
        self.cache.insert(id.clone(), at);
        at
    }
}

/// The tracker project an agent works in: the longest declared repo
/// path its cwd sits under (worktrees under `.cadence/wt/` included).
/// "" — a global row — when nothing matches.
fn agent_project(a: &Value, repos: &[(PathBuf, String)]) -> String {
    let Some(cwd) = a["cwd"].as_str().filter(|c| !c.is_empty()) else {
        return String::new();
    };
    let cwd = PathBuf::from(cwd);
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    repos
        .iter()
        .filter(|(path, _)| cwd.starts_with(path))
        .max_by_key(|(path, _)| path.components().count())
        .map(|(_, key)| key.clone())
        .unwrap_or_default()
}

/// The needs-me rows one agent contributes: its `agent_list` row (state,
/// stall view, mailbox backlog and health) plus its probe.
/// CAD-439 (operator decision): a confined master whose own Claude
/// config dir holds no login cannot authenticate — one info row naming
/// the command that gives it its own, until the login exists.
fn master_login_item(a: &Value, state_dir: &Path, project: &str, now: i64) -> Option<Item> {
    let alias = a["alias"].as_str().unwrap_or_default();
    let confined =
        crate::master::is_confined(Some(&a["params"]), crate::confine::available().is_ok());
    if !crate::master::is_master(alias)
        || !confined
        || a["state"].as_str() == Some("stopped")
        || crate::master::has_login(state_dir)
    {
        return None;
    }
    let command = crate::master::login_command(state_dir);
    let age = now - a["updated"].as_f64().unwrap_or(now as f64) as i64;
    Some(
        item(
            90,
            "master_login",
            &format!("master has no Claude login — give it its own: {command}"),
            age,
            project,
            None,
            &command,
        )
        .about("agent", alias)
        .for_agent(alias)
        .owned_by(None),
    )
}

fn agent_items(a: &Value, probe: &AgentProbe, project: &str, now: i64) -> Vec<Item> {
    let alias = a["alias"].as_str().unwrap_or_default();
    let age = now - a["updated"].as_f64().unwrap_or(now as f64) as i64;
    // The agent's PM acts on its rows (CAD-253); a root agent has none.
    let pm = a["params"]["upstream"].as_str();
    let row = |rank: u8, kind: &str, title: &str, age: i64, command: &str| {
        item(rank, kind, title, age, project, None, command)
            .about("agent", alias)
            .for_agent(alias)
            .owned_by(pm)
    };
    let mut items = Vec::new();
    // CAD-439 (operator decision): a master started `--unconfined` on a
    // host without Landlock shows for as long as it is registered and
    // not stopped.
    if crate::master::is_master(alias)
        && crate::master::unconfined(Some(&a["params"]))
        && a["state"].as_str() != Some("stopped")
    {
        items.push(
            row(
                90,
                "master_unconfined",
                "master runs unconfined — no filesystem sandbox on this host; it can read and \
                 write your files",
                age,
                &cmd_agent_show(alias),
            )
            .owned_by(None),
        );
    }
    // Condition clocks (CAD-253): a daemon-measured age is a start
    // time; `updated` is not — any params/model write moves it.
    let secs_ago = |key: &str| a[key].as_f64().map(|s| now - s as i64);
    // CAD-413: an auto-resume for queued work failed — the agent is
    // down with a message waiting. The more specific row: it replaces
    // the generic `fenced` row a failed open would otherwise raise.
    let resume_failed = &a["auto_resume_failed"];
    if resume_failed.is_object() {
        let at = resume_failed["at"].as_f64().map(|at| at as i64);
        items.push(
            row(
                30,
                "auto_resume_failed",
                &format!(
                    "agent {alias} auto-resume failed — message {} waiting: {}",
                    resume_failed["message"].as_str().unwrap_or("?"),
                    resume_failed["reason"].as_str().unwrap_or("unknown error"),
                ),
                at.map_or(age, |at| now - at),
                &cmd_agent_resume(alias),
            )
            .since(at),
        );
    } else if a["state"].as_str() == Some("attention") {
        items.push(
            row(
                30,
                "fenced",
                &format!("agent {alias} fenced — reconcile then resume"),
                age,
                &cmd_agent_unfence(alias),
            )
            // Operator only (CAD-374): the PM escalates, it cannot act.
            .owned_by(None)
            .since(fenced_since(a, probe)),
        );
    }
    if a["stalled"].as_bool().unwrap_or(false) {
        items.push(
            row(
                40,
                "stalled",
                &format!("agent {alias} turn silent"),
                a["silent_secs"].as_f64().unwrap_or(age as f64) as i64,
                &cmd_agent_show(alias),
            )
            .since(secs_ago("silent_secs")),
        );
    }
    // A sampled approval menu ranks with brokered approvals — the pane
    // is waiting on a human either way.
    if let Some(line) = a["pane_menu"].as_str() {
        items.push(row(
            20,
            "approval_menu",
            &format!("agent {alias} approval menu: {line}"),
            age,
            &cmd_agent_answer(alias),
        ));
    }
    // CAD-250: an unreported turn holds the actor's queue — a row only
    // once real work waits behind it, so a healthy turn in progress is
    // never needs-me noise. Past the bound the turn goes `unknown` and
    // the `fenced` row takes over.
    let awaiting = &a["awaiting_report"];
    let behind = awaiting["queued_behind"].as_i64().unwrap_or(0);
    if awaiting.is_object() && behind > 0 {
        let waited = awaiting["since_secs"].as_u64().unwrap_or(0);
        let bound = match awaiting["remaining_secs"].as_u64() {
            Some(left) => format!("unknown in {}", inbox::fmt_age(left)),
            None => "no report bound set".to_string(),
        };
        items.push(
            row(
                40,
                "awaiting_report",
                &format!(
                    "agent {alias} awaiting report for {} — {behind} queued behind it; {bound}",
                    inbox::fmt_age(waited)
                ),
                waited as i64,
                &cmd_agent_show(alias),
            )
            .since(Some(now - waited as i64)),
        );
    }
    if a["silent_ended"].as_bool().unwrap_or(false) {
        items.push(
            row(
                40,
                "silent_end",
                &format!("agent {alias} turn ended at an idle pane — never reported"),
                a["ended_secs"].as_f64().unwrap_or(age as f64) as i64,
                &cmd_send_nudge(alias),
            )
            .since(secs_ago("ended_secs")),
        );
    }
    let queued = a["inbox"]["queued"]
        .as_i64()
        .or_else(|| probe.show.as_ref().and_then(|s| s["queued"].as_i64()))
        .unwrap_or(0);
    if a["provider"].as_str() == Some(registry::INBOX) && queued > 0 {
        items.push(row(
            100,
            "inbox_unread",
            &format!("{queued} unread for {alias}"),
            age,
            &cmd_inbox(alias),
        ));
    }
    // CAD-251: a mailbox past its unread threshold with no recent
    // `inbox_read`, attributed to its owner (group root, else operator).
    let health = &a["inbox_health"];
    if health["stale"].as_bool().unwrap_or(false) {
        let owner = health["owner"].as_str().unwrap_or(inbox::OPERATOR);
        let oldest = health["oldest_unread_age_secs"].as_i64().unwrap_or(0);
        let mut stale = row(
            95,
            "inbox_stale",
            &format!(
                "inbox {alias} has no consumer — {} unread, oldest {}, owner {owner}",
                health["unread"].as_u64().unwrap_or(0),
                inbox::fmt_age(oldest.max(0) as u64),
            ),
            oldest,
            &cmd_inbox(alias),
        )
        .for_agent(owner)
        .owned_by(Some(owner))
        .since(
            health["oldest_unread_age_secs"]
                .as_i64()
                .map(|secs| now - secs),
        );
        stale.json["owner"] = json!(owner);
        items.push(stale);
    }
    for req in &probe.requests {
        let handle = req["request"].as_str().unwrap_or_default();
        let method = req["method"].as_str().unwrap_or("request");
        let what = if method == "item/tool/requestUserInput" {
            "input request"
        } else {
            "approval"
        };
        items.push(row(
            20,
            "approval",
            &format!("{method} {what} for {alias}"),
            age,
            &cmd_agent_respond(alias, handle, method),
        ));
    }
    items
}

/// `(earliest start, latest finish)` over a PR head's rollup, epoch
/// secs: a CheckRun contributes `startedAt` and `completedAt`, a status
/// context its `startedAt` (when it was posted).
fn rollup_span(rollup: &[Value]) -> (Option<i64>, Option<i64>) {
    // gh reports a not-yet-started check as `0001-01-01T00:00:00Z`.
    let at = |c: &Value, key: &str| c[key].as_str().and_then(parse_iso).filter(|t| *t > 0);
    let starts = rollup.iter().filter_map(|c| at(c, "startedAt"));
    let ends = rollup
        .iter()
        .filter_map(|c| at(c, "completedAt").or_else(|| at(c, "startedAt")));
    (starts.min(), ends.max())
}

/// A fence began when its turn went `unknown`: the earliest `completed`
/// among the agent's unknown messages (the probe's `agent_show`). A
/// fence with no such message (a provider disconnect while idle, a
/// restart mismatch) falls back to the row's `updated`: every write
/// that enters `attention` stamps it and no unstamped write does, so an
/// agent still in `attention` has held it at least since `updated` — a
/// later params or model write only shortens the clock, never inflates it.
fn fenced_since(a: &Value, probe: &AgentProbe) -> Option<i64> {
    let unknown = probe
        .show
        .as_ref()
        .and_then(|show| show["messages"].as_array())
        .and_then(|ms| {
            ms.iter()
                .filter(|m| m["state"].as_str() == Some("unknown"))
                .filter_map(|m| m["completed"].as_f64())
                .map(|t| t as i64)
                .min()
        });
    unknown.or_else(|| a["updated"].as_f64().map(|t| t as i64))
}

/// Scope the merged rows: `--project` keeps rows attributed to the key,
/// `--group` keeps rows owned by the root or a member.
fn scope_rows(items: Vec<Item>, project: Option<&str>, members: Option<&[String]>) -> Vec<Item> {
    items
        .into_iter()
        .filter(|i| project.is_none_or(|p| i.json["project"].as_str() == Some(p)))
        .filter(|i| members.is_none_or(|m| i.agents.iter().any(|a| m.contains(a))))
        .collect()
}

/// A group's aliases — the root plus every agent whose upstream names
/// it (one level, like `cadence status --group`). An alias no agent
/// carries is an error, not an empty screen.
fn group_members(view: &DaemonView, root: &str) -> Result<Vec<String>, String> {
    if !view.reachable {
        return Err(format!("cannot resolve group '{root}': daemon unreachable"));
    }
    if !view
        .agents
        .iter()
        .any(|a| a["alias"].as_str() == Some(root))
    {
        return Err(format!(
            "unknown group '{root}' — no registered agent has that alias (see `cadence agent list`)"
        ));
    }
    Ok(view
        .agents
        .iter()
        .filter(|a| {
            a["alias"].as_str() == Some(root) || a["params"]["upstream"].as_str() == Some(root)
        })
        .filter_map(|a| a["alias"].as_str().map(str::to_string))
        .collect())
}

/// A claim's age through the read model's clock cache: a recorded claim
/// carries its own time; an owner-only issue's `git log` answer is kept
/// per `HEAD` and `issue.md` rev.
fn claim_since(
    clock: &claim::Clock,
    shared: Option<SharedClocks<'_>>,
    project: &str,
    v: &board::View,
) -> Option<i64> {
    let front = &v.issue.front;
    let keyed = shared
        .filter(|_| front.claim.is_none())
        .and_then(|c| Some((c, c.key(v)?)));
    let Some((c, key)) = keyed else {
        return clock.since(project, front);
    };
    if let Some(at) = c.get("claim", &front.id, &key) {
        return Some(at);
    }
    let at = clock.since(project, front);
    c.put("claim", &front.id, key, at);
    at
}

/// Build the screen under `opts`. The daemon probes, the gh refresh and
/// the tracker read run concurrently — each bounded — so the view costs
/// its slowest source, not their sum. Fails only on a scope naming an
/// unknown project key or group.
pub fn overview_with(state_dir: &Path, pm_dir: &Path, opts: &Options) -> Result<Value, String> {
    overview_from(state_dir, pm_dir, opts, None)
}

/// The board read model's build (CAD-325): the tracker, daemon and clock
/// inputs come from `reuse`; only `gh` (from its cache, waiting at most
/// `gh_wait`) and the local git reads run here.
pub fn overview_board_from(
    state_dir: &Path,
    pm_dir: &Path,
    reuse: Reuse<'_>,
    gh_wait: Duration,
) -> Value {
    let opts = Options {
        gh_wait,
        ..Options::board()
    };
    unscoped(overview_from(state_dir, pm_dir, &opts, Some(reuse)))
}

fn overview_from(
    state_dir: &Path,
    pm_dir: &Path,
    opts: &Options,
    reuse: Option<Reuse<'_>>,
) -> Result<Value, String> {
    let now = now_epoch();
    let pm = issue::Pm::at(pm_dir).ok();
    let projects = pm
        .as_ref()
        .map(|pm| project::list(&pm.dir).unwrap_or_default())
        .unwrap_or_default();
    if let Some(key) = opts.scope.project.as_deref() {
        if !projects.iter().any(|p| p.key == key) {
            let known: Vec<&str> = projects.iter().map(|p| p.key.as_str()).collect();
            return Err(if known.is_empty() {
                format!(
                    "unknown project '{key}' — no tracker projects under {}",
                    pm_dir.display()
                )
            } else {
                format!("unknown project '{key}' — known: {}", known.join(", "))
            });
        }
    }
    let mut slugs = Vec::new();
    let mut slug_project: HashMap<String, String> = HashMap::new();
    let mut repo_paths: Vec<(PathBuf, String)> = Vec::new();
    // Slug → the declared local clone its first-parent log comes from.
    let mut slug_clone: HashMap<String, PathBuf> = HashMap::new();
    for p in &projects {
        for r in &p.repos {
            let path = r.path.as_deref().map(|path| {
                let path = project::expand_home(path);
                path.canonicalize().unwrap_or(path)
            });
            if let Some(remote) = &r.remote {
                let norm = project::normalize_remote(remote);
                if let Some(slug) = norm.strip_prefix("github.com/") {
                    slug_project.insert(slug.to_string(), p.key.clone());
                    slugs.push(slug.to_string());
                    if let Some(path) = &path {
                        slug_clone
                            .entry(slug.to_string())
                            .or_insert_with(|| path.clone());
                    }
                }
            }
            if let Some(path) = path {
                repo_paths.push((path, p.key.clone()));
            }
        }
    }
    slugs.sort();
    slugs.dedup();

    // ---- the three sources, concurrently (what `reuse` lacks) ----
    let reused = reuse.as_ref();
    let (fresh_sources, (gh_repos, gh_state), fresh_views) = std::thread::scope(|s| {
        let sources = reused
            .is_none()
            .then(|| s.spawn(|| daemon_sources(state_dir, opts)));
        let gh = s.spawn(|| {
            if opts.cache_only {
                github_repos_cached(state_dir, &slugs)
            } else {
                github_bounded(state_dir, &slugs, opts.gh_wait, gh_repo)
            }
        });
        // Local files only — the notes index keeps it one pass.
        let tracker = reused.is_none().then(|| {
            s.spawn(|| {
                pm.as_ref().map(|pm| {
                    let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                    board::views(&pm.config.notes_dir(), issues)
                })
            })
        });
        (
            sources.map(|h| h.join().expect("overview daemon sources panicked")),
            gh.join().expect("overview gh refresh panicked"),
            tracker.and_then(|h| h.join().expect("overview tracker read panicked")),
        )
    });
    let sources = match (reused, &fresh_sources) {
        (Some(r), _) => r.sources,
        (None, Some(fresh)) => fresh,
        (None, None) => unreachable!("sources are gathered unless reused"),
    };
    let daemon = &sources.daemon;
    let monitoring_view = sources.monitoring.clone();
    let views: Option<&[board::View]> = match reused {
        Some(r) => pm.as_ref().map(|_| r.views),
        None => fresh_views.as_deref(),
    };
    // The clock cache keys on the tracker's HEAD — one read per build; no
    // HEAD (not a repo, git failing) means no cross-build cache.
    let head = reused.and(pm.as_ref()).and_then(|pm| {
        git_text(&pm.dir, &["rev-parse".into(), "HEAD".into()])
            .ok()
            .map(|h| h.trim().to_string())
    });
    let clocks = reused.zip(head.as_deref()).map(|(r, head)| SharedClocks {
        cache: r.clocks,
        head,
    });
    let mut degraded_notes = daemon.degraded.clone();
    if !slugs.is_empty() {
        if let Some(e) = gh_state["error"].as_str() {
            degraded_notes.push(degraded("github", "", e));
        }
    }
    let members = match opts.scope.group.as_deref() {
        Some(root) => Some(group_members(daemon, root)?),
        None => None,
    };

    // ---- daemon rows ----
    let mut needs: Vec<Item> = Vec::new();
    let mut panes_idle = daemon.reachable;
    // An old daemon without `agent_probe` cannot confirm a pane is idle
    // — the drift row must say so instead of vanishing quietly.
    let mut probes_unknown = false;
    for (a, probe) in daemon.agents.iter().zip(&daemon.probes) {
        let project = agent_project(a, &repo_paths);
        needs.extend(agent_items(a, probe, &project, now));
        needs.extend(master_login_item(a, state_dir, &project, now));
        panes_idle &= !probe.holds_drift;
        probes_unknown |= probe.probe_unknown;
    }

    // ---- tracker rows ----
    // Lowercased headRefName of every open PR — an issue in review
    // counts as "has a PR" when a `cadence/<id-lowercase>-…` branch is
    // open, even without an explicit `pr` ref.
    let mut open_pr_branches: Vec<String> = Vec::new();
    for data in gh_repos.values() {
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            if let Some(head) = pr["headRefName"].as_str() {
                open_pr_branches.push(head.to_lowercase());
            }
        }
    }
    // `cadence/<id-lowercase>-` branch prefix → (issue id, owner): a
    // PR row belongs to the issue's owner for `--group`.
    let mut branch_issue: Vec<(String, String, Option<String>)> = Vec::new();
    let mut projects_out = Vec::new();
    if let (Some(pm), Some(views)) = (&pm, views) {
        let mut status_of: HashMap<String, String> = HashMap::new();
        for v in views {
            status_of.insert(v.issue.front.id.clone(), v.status.clone());
            branch_issue.push((
                crate::worktree::layout::issue_branch_prefix(&v.issue.front.id),
                v.issue.front.id.clone(),
                v.issue.front.owner.clone(),
            ));
        }
        let view_of: HashMap<&str, &board::View> = views
            .iter()
            .map(|v| (v.issue.front.id.as_str(), v))
            .collect();
        let mut clock = StatusClock::new(&pm.dir, STATUS_CLOCK_BUDGET, clocks);
        let mut intake: Vec<Item> = Vec::new();
        let escalations = crate::master::escalations(state_dir);
        for v in views {
            let id = v.issue.front.id.as_str();
            let project = v.issue.project.as_str();
            let owner = v.issue.front.owner.as_deref().unwrap_or_default();
            let age = parse_iso(&v.issue.front.created)
                .map(|c| now - c)
                .unwrap_or(0);
            let branch_prefix = crate::worktree::layout::issue_branch_prefix(id);
            let open_pr = v
                .issue
                .front
                .refs
                .iter()
                .any(|r| r.kind == "pr" && r.closed != Some(true))
                || open_pr_branches
                    .iter()
                    .any(|b| b.starts_with(&branch_prefix));
            if v.status == "review" && !open_pr {
                needs.push(
                    item(
                        70,
                        "review_no_pr",
                        &format!("{id} in review with no open PR"),
                        age,
                        project,
                        None,
                        &cmd_issue_show(id),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner))
                    .since(clock.since(v)),
                );
            }
            let unblocked = !v.issue.front.blocked_by.is_empty()
                && v.issue
                    .front
                    .blocked_by
                    .iter()
                    .all(|b| status_of.get(b).map(String::as_str) == Some("done"));
            if !matches!(v.status.as_str(), "done" | "dropped") && unblocked {
                // Unblocked when the last blocker reached done; one
                // blocker without a clock leaves the row without one.
                let since = v
                    .issue
                    .front
                    .blocked_by
                    .iter()
                    .map(|b| view_of.get(b.as_str()).and_then(|bv| clock.since(bv)))
                    .collect::<Option<Vec<i64>>>()
                    .and_then(|ts| ts.into_iter().max());
                needs.push(
                    item(
                        80,
                        "blocked_ready",
                        &format!("{id} unblocked — blockers all done"),
                        age,
                        project,
                        None,
                        &cmd_issue_set_ready(id),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner))
                    .since(since),
                );
            }
            // CAD-339 Needs-you: a plan waiting for the operator's
            // decision, and every open question the master escalated —
            // with the master's summary, the question and its options.
            if let Some(plan) = v
                .issue
                .front
                .plan
                .as_ref()
                .filter(|p| p.state == "proposed")
            {
                let since = parse_iso(&plan.proposed_at);
                let mut row = item(
                    25,
                    "plan",
                    &format!(
                        "{id} plan proposed by {} — {} ({} tickets)",
                        plan.proposed_by,
                        v.issue.front.title,
                        plan.tickets.len()
                    ),
                    since.map_or(age, |t| now - t),
                    project,
                    None,
                    &format!("cadence plan show {id} && cadence plan approve {id}"),
                )
                .about("issue", id)
                .since(since);
                row.json["plan"] = json!({"epic": id, "proposed_by": plan.proposed_by,
                                          "tickets": plan.tickets});
                needs.push(row);
            }
            // Only the daemon's escalation record puts a question here —
            // a report file never can (review round 1, I3); reports are
            // parsed only for tickets that have one.
            let escalated_here = escalations.keys().any(|k| k.starts_with(&format!("{id}/")));
            let open = if escalated_here {
                issue::task_report::open_questions(&v.issue.dir, id)
            } else {
                vec![]
            };
            for q in open {
                let key = format!("{id}/{}", q["name"].as_str().unwrap_or_default());
                let Some(up) = escalations.get(&key).and_then(Value::as_object) else {
                    continue;
                };
                let since = q["at"].as_str().and_then(parse_iso);
                let mut row = item(
                    20,
                    "question",
                    &format!(
                        "{id} question from {} — {}",
                        q["agent"].as_str().unwrap_or_default(),
                        q["impact"].as_str().unwrap_or_default()
                    ),
                    since.map_or(age, |t| now - t),
                    project,
                    None,
                    &format!(
                        "cadence issue show {id}  # answer: cadence report file --task {id} \
                         --kind answer (answers: {})",
                        q["name"].as_str().unwrap_or_default()
                    ),
                )
                .about(
                    "report",
                    &format!("{id}/{}", q["name"].as_str().unwrap_or_default()),
                )
                .for_agent(q["agent"].as_str().unwrap_or_default())
                .since(since);
                row.json["question"] = json!({
                    "issue": id, "report": q["name"], "agent": q["agent"],
                    "options": q["options"], "impact": q["impact"], "body": q["body"],
                });
                row.json["summary"] = up.get("summary").cloned().unwrap_or(Value::Null);
                row.json["escalated_by"] = up.get("by").cloned().unwrap_or(Value::Null);
                needs.push(row);
            }
            // `cadence report` intake: a backlog-tagged row surfaces
            // until triage moves it off backlog — the effective status
            // (notes-derived counts too) is what clears it.
            if v.status == "backlog" && v.issue.front.tags.iter().any(|t| t == "intake") {
                let kind_tag = v
                    .issue
                    .front
                    .kind
                    .as_deref()
                    .or_else(|| {
                        v.issue
                            .front
                            .tags
                            .iter()
                            .find(|t| *t != "intake")
                            .map(String::as_str)
                    })
                    .unwrap_or("intake");
                intake.push(
                    item(
                        85,
                        "intake",
                        &format!("{id} {kind_tag} report — {}", v.issue.front.title),
                        age,
                        project,
                        None,
                        &format!("cadence report show {id}"),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner)),
                );
            }
        }
        // Cap the intake block — hundreds of untriaged reports must not
        // bury real work. Oldest first, then one summary row.
        intake.sort_by_key(|i| std::cmp::Reverse(i.age));
        let intake_extra = intake
            .len()
            .checked_sub(report::NEEDS_ME_CAP)
            .filter(|n| *n > 0);
        if intake_extra.is_some() {
            intake.truncate(report::NEEDS_ME_CAP);
        }
        // Clocks only for the rows that surface — one read each.
        for it in &mut intake {
            let since = view_of
                .get(it.subject.1.as_str())
                .and_then(|v| clock.since(v));
            it.set_since(since);
        }
        if let Some(extra) = intake_extra {
            intake.push(
                item(
                    85,
                    "intake",
                    &format!("… {extra} more intake reports"),
                    0,
                    "",
                    None,
                    "cadence report ls",
                )
                .about("report", "intake-overflow"),
            );
        }
        needs.extend(intake);
        needs.extend(delivery_items(state_dir, now));
        if clock.skipped > 0 {
            degraded_notes.push(degraded(
                "tracker_status_time",
                "",
                format!(
                    "{} issue(s) past the status-time budget — those rows escalate by owner only",
                    clock.skipped
                ),
            ));
        }
        // CAD-383: in-flight claims per project, with their age.
        let claim_clock = claim::Clock::new(&pm.dir, STATUS_CLOCK_BUDGET);
        for p in &projects {
            if opts.scope.project.as_deref().is_some_and(|k| k != p.key) {
                continue;
            }
            let mut open_by_status = serde_json::Map::new();
            let mut oldest_review: Option<i64> = None;
            let mut claims: Vec<Value> = Vec::new();
            for v in views.iter().filter(|v| v.issue.project == p.key) {
                if matches!(v.status.as_str(), "done" | "dropped") {
                    continue;
                }
                let front = &v.issue.front;
                if matches!(v.status.as_str(), "doing" | "review")
                    && !claim::holders(front).is_empty()
                {
                    let since = claim_since(&claim_clock, clocks, &p.key, v);
                    let mut row = claim::row(&p.key, front, since, now);
                    row["status"] = json!(v.status);
                    claims.push(row);
                }
                let n = open_by_status
                    .get(&v.status)
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                open_by_status.insert(v.status.clone(), json!(n + 1));
                if v.status == "review" {
                    let age = parse_iso(&v.issue.front.created)
                        .map(|c| now - c)
                        .unwrap_or(0);
                    oldest_review = Some(oldest_review.map_or(age, |o| o.max(age)));
                }
            }
            projects_out.push(json!({
                "key": p.key,
                "open_by_status": open_by_status,
                "oldest_review_age": oldest_review,
                "claims": claims,
            }));
        }
        // Tracker behind its upstream — local refs only, never a fetch.
        if let Ok(behind) = git_text(
            &pm.dir,
            &[
                "rev-list".into(),
                "--count".into(),
                "HEAD..@{upstream}".into(),
            ],
        ) {
            if let Ok(n) = behind.trim().parse::<i64>() {
                if n > 0 {
                    needs.push(
                        item(
                            110,
                            "tracker_behind",
                            &format!("tracker {n} commit(s) behind upstream"),
                            0,
                            "",
                            None,
                            CMD_ISSUE_SYNC,
                        )
                        .about("tracker", "pm"),
                    );
                }
            }
        }
    }

    // ---- GitHub rows: merge-ready, verdict-less, main CI ----
    let mut main_ci: Vec<Value> = Vec::new();
    for (slug, data) in &gh_repos {
        let project = slug_project.get(slug).cloned().unwrap_or_default();
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            let n = pr["number"].as_i64().unwrap_or(0);
            let title = pr["title"].as_str().unwrap_or("");
            let url = pr["url"].as_str().map(str::to_string);
            let rollup = pr["statusCheckRollup"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let age = pr["updatedAt"]
                .as_str()
                .and_then(parse_iso)
                .map(|u| now - u)
                .unwrap_or(0);
            let head_ref = pr["headRefName"].as_str().unwrap_or("").to_lowercase();
            let owner = branch_issue
                .iter()
                .find(|(prefix, _, _)| head_ref.starts_with(prefix.as_str()))
                .and_then(|(_, _, owner)| owner.clone())
                .unwrap_or_default();
            let subject = format!("{slug}#{n}");
            // Rollup times for this head (CAD-253): a CheckRun's
            // `completedAt`, a status context's `startedAt`.
            let (first_check, last_check) = rollup_span(&rollup);
            match verdict_state(&rollup).as_deref() {
                Some("SUCCESS") if checks_green(&rollup) => {
                    // The verdict binds this head — a push after it
                    // makes the copied command refuse instead of
                    // merging an unreviewed head.
                    let head = pr["headRefOid"].as_str().unwrap_or("");
                    needs.push(
                        item(
                            10,
                            "merge",
                            &format!("PR #{n} {title} — verdict pass, checks green"),
                            age,
                            &project,
                            url.as_deref(),
                            &format!(
                                "gh pr merge {n} --repo {slug} --squash --admin --match-head-commit {head}"
                            ),
                        )
                        .about("pr", &subject)
                        .for_agent(&owner)
                        .owned_by(Some(&owner))
                        // Merge-ready once the last check or verdict landed.
                        .since(last_check),
                    );
                }
                Some("SUCCESS") | Some("FAILURE") | Some("ERROR") => {}
                _ => needs.push(
                    item(
                        60,
                        "pr_no_verdict",
                        &format!("PR #{n} {title} — no verdict"),
                        age,
                        &project,
                        url.as_deref(),
                        &format!("gh pr view {n} --repo {slug}"),
                    )
                    .about("pr", &subject)
                    .for_agent(&owner)
                    .owned_by(Some(&owner))
                    // Verdict-less since this head's first check started;
                    // a head with no checks yet has no clock.
                    .since(first_check),
                ),
            }
        }
        let clone = slug_clone.get(slug).map(PathBuf::as_path);
        let (view, rows) = main_ci_view(slug, &project, data, clone, now);
        if let Some(e) = view["error"].as_str() {
            degraded_notes.push(degraded("github_ci", slug, e));
        }
        if !view.is_null() && opts.scope.project.as_deref().is_none_or(|k| k == project) {
            main_ci.push(view);
        }
        needs.extend(rows);
    }
    main_ci.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    // ---- deploy drift: is what we merged actually running ----
    let info = daemon.info.clone();
    let mut drift = if !daemon.reachable {
        json!({"matched": false, "reason": "daemon unreachable — cannot tell"})
    } else if info.is_none() {
        // Reachable but predates `daemon_info` — the build commit is
        // unreadable, so drift is unknowable, never zero.
        json!({
            "matched": false,
            "reason": "daemon build unknown (daemon predates daemon_info) — restart to enable drift",
        })
    } else {
        match build_repo_match(&projects) {
            None => json!({
                "matched": false,
                "build_commit": info.as_ref().map(|i| i["build_commit"].clone()).unwrap_or(json!("unknown")),
                "reason": "no tracker repo matches the build — cannot tell",
            }),
            Some((key, repo)) => {
                let build = info
                    .as_ref()
                    .and_then(|i| i["build_commit"].as_str())
                    .unwrap_or("unknown");
                let mut d = compute_drift(&repo, build);
                d["matched"] = json!(true);
                d["project"] = json!(key);
                d
            }
        }
    };
    if drift["known"].as_bool().unwrap_or(false) && drift["count"].as_i64().unwrap_or(0) > 0 {
        if panes_idle {
            let n = drift["count"].as_i64().unwrap_or(0);
            let project = drift["project"].as_str().unwrap_or("");
            needs.push(
                item(
                    50,
                    "drift",
                    &format!("{n} merged commit(s) not running — all panes idle"),
                    0,
                    project,
                    None,
                    CMD_UPGRADE_LATEST_MAIN,
                )
                .about("deploy", project),
            );
        } else {
            // Held back, but say why — a busy pane is different from
            // a daemon that cannot answer `agent_probe` at all.
            drift["held"] = json!(if probes_unknown {
                "cannot tell whether panes are idle — daemon predates agent_probe"
            } else {
                "a pane is busy — restart only when idle"
            });
        }
    }

    classify_needs(
        &mut needs,
        &Owners::new(daemon.reachable, &daemon.agents),
        now,
        ESCALATE_AFTER_SECS,
    );
    let mut needs = scope_rows(
        merge_by_subject(needs),
        opts.scope.project.as_deref(),
        members.as_deref(),
    );
    sort_needs(&mut needs);
    let daemon_json = match info {
        Some(mut i) => {
            i["reachable"] = json!(daemon.reachable);
            i
        }
        None if daemon.reachable => json!({
            "reachable": true,
            "info": "daemon predates daemon_info — build identity unreadable",
        }),
        None => json!({"reachable": false}),
    };
    Ok(json!({
        "needs_me": needs.iter().map(|i| i.json.clone()).collect::<Vec<_>>(),
        "drift": drift,
        "projects": projects_out,
        "github": gh_state,
        "main_ci": main_ci,
        "daemon": daemon_json,
        "monitoring": monitoring_view,
        "degraded": degraded_notes,
        "scope": {"project": opts.scope.project, "group": opts.scope.group},
        "generated_at": now,
    }))
}

// ---------- shared with `cadence session` ----------

/// `session`'s reconcile/handoff share the gh fetch (and its cache)
/// rather than re-running `gh pr list` — same slug set, same data.
pub(crate) fn github_repos(state_dir: &Path, slugs: &[String]) -> (HashMap<String, Value>, Value) {
    github(state_dir, slugs)
}

/// The cache body only — never fetches, never writes. `session end
/// --dry-run` must write nothing at all, cache included, so it reads
/// what a previous real fetch left and reports `unavailable` when the
/// cache is empty or covers a different slug set.
pub(crate) fn github_repos_cached(
    state_dir: &Path,
    slugs: &[String],
) -> (HashMap<String, Value>, Value) {
    match read_cache(&cache_file(state_dir)) {
        Some(c) if c.slugs == slugs => (
            c.repos,
            json!({"state": "cached", "at": c.at, "as_of": c.at}),
        ),
        _ => (HashMap::new(), json!({"state": "unavailable"})),
    }
}

/// Drift of an arbitrary commit against the repo's default ref —
/// `session start` measures the *binary* build this way.
pub(crate) fn drift_of(repo: &Path, commit: &str) -> Value {
    compute_drift(repo, commit)
}

/// Which tracker project owns the repo this binary was built from.
pub(crate) fn build_repo_match_pub(projects: &[project::Project]) -> Option<(String, PathBuf)> {
    build_repo_match(projects)
}

/// Verdict + checks on a PR rollup, for the session-end handoff.
pub(crate) fn verdict_state_pub(rollup: &[Value]) -> Option<String> {
    verdict_state(rollup)
}

pub(crate) fn checks_green_pub(rollup: &[Value]) -> bool {
    checks_green(rollup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_parses() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2000-01-01T00:00:00Z"), Some(946684800));
        assert_eq!(parse_iso("2026-09-19T05:33:43"), None);
        assert_eq!(parse_iso(""), None);
        assert_eq!(parse_iso("not a date at all!!!!"), None);
    }

    #[test]
    fn pr_numbers() {
        assert_eq!(pr_number("board: tailnet sharing (#51)"), Some(51));
        assert_eq!(pr_number("no number"), None);
        assert_eq!(pr_number("mid (#5) subject"), None);
        assert_eq!(pr_number("edge (#abc)"), None);
    }

    #[test]
    fn rollup_verdict_and_green() {
        let rollup = vec![
            json!({"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}),
            json!({"__typename":"StatusContext","context":"qa-verdict","state":"SUCCESS"}),
        ];
        assert_eq!(verdict_state(&rollup).as_deref(), Some("SUCCESS"));
        assert!(checks_green(&rollup));
        let red = vec![
            json!({"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"}),
            json!({"__typename":"StatusContext","context":"qa-verdict","state":"SUCCESS"}),
        ];
        assert!(!checks_green(&red));
        let pending =
            vec![json!({"__typename":"StatusContext","context":"qa-verdict","state":"PENDING"})];
        assert!(checks_green(&pending));
        assert_eq!(verdict_state(&pending).as_deref(), Some("PENDING"));
        assert_eq!(verdict_state(&[]), None);
        assert!(checks_green(&[]));
    }

    #[test]
    fn ordering_rank_then_age() {
        let mut v = vec![
            item(60, "pr_no_verdict", "b", 5, "", None, "c"),
            item(10, "merge", "a", 1, "", None, "c"),
            item(60, "pr_no_verdict", "c", 50, "", None, "c"),
        ];
        sort_needs(&mut v);
        let kinds: Vec<&str> = v.iter().map(|i| i.json["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["merge", "pr_no_verdict", "pr_no_verdict"]);
        assert_eq!(v[1].json["title"], "c"); // older first within a kind
    }

    /// CAD-252: an agent that is both stalled and silently ended is one
    /// row with two causes, most severe first; other subjects stay put.
    #[test]
    fn stalled_and_silent_end_merge_into_one_row() {
        let now = 1_000_000;
        let agent = json!({
            "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
            "state": "busy", "updated": (now - 30) as f64,
            "stalled": true, "silent_secs": 900,
            "silent_ended": true, "ended_secs": 600,
        });
        let mut rows = agent_items(&agent, &AgentProbe::default(), "cadence", now);
        assert_eq!(rows.len(), 2, "two raw causes");
        rows.push(
            item(
                70,
                "review_no_pr",
                "CAD-1 in review",
                5,
                "cadence",
                None,
                "c",
            )
            .about("issue", "CAD-1"),
        );
        let merged = merge_by_subject(rows);
        assert_eq!(merged.len(), 2, "one row per subject");
        let w1 = &merged[0].json;
        assert_eq!(w1["subject"], json!({"kind": "agent", "id": "w1"}));
        assert_eq!(w1["kind"], "stalled", "primary cause keeps `kind`");
        assert_eq!(w1["cause"], "stalled");
        let causes: Vec<&str> = w1["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["cause"].as_str().unwrap())
            .collect();
        assert_eq!(causes, ["stalled", "silent_end"]);
        assert_eq!(w1["causes"][1]["command"], cmd_send_nudge("w1"));
        assert_eq!(w1["project"], "cadence");
        // A lone row still carries its one cause.
        assert_eq!(merged[1].json["causes"].as_array().unwrap().len(), 1);
        assert_eq!(merged[1].json["subject"]["kind"], "issue");
    }

    /// Severity decides the primary cause, not emit order; a stale inbox
    /// merges with its unread row and names its owner.
    #[test]
    fn merge_orders_causes_by_severity_and_keeps_stale_owner() {
        let now = 1_000_000;
        let inbox = json!({
            "alias": "obs", "provider": "inbox", "endpoint_kind": "inbox",
            "state": "idle", "updated": now as f64,
            "inbox": {"queued": 60},
            "inbox_health": {"stale": true, "unread": 60,
                             "oldest_unread_age_secs": 90_000, "owner": "pm"},
        });
        let merged = merge_by_subject(agent_items(&inbox, &AgentProbe::default(), "", now));
        assert_eq!(merged.len(), 1);
        let row = &merged[0];
        assert_eq!(row.json["kind"], "inbox_stale", "{}", row.json);
        assert_eq!(row.json["owner"], "pm");
        assert!(row.json["title"].as_str().unwrap().contains("60 unread"));
        let causes: Vec<&str> = row.json["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["cause"].as_str().unwrap())
            .collect();
        assert_eq!(causes, ["inbox_stale", "inbox_unread"]);
        // Attributed to the owner for `--group pm`, and to the inbox.
        assert_eq!(row.agents, ["obs", "pm"]);
    }

    // ---- CAD-253: needs-me audience from owner liveness and the
    // unhandled clock ----

    const NOW: i64 = 1_000_000;

    /// A fenced worker `w1` whose PM is `pm`: its turn went `unknown`
    /// `fenced_ago` seconds before [`NOW`]. Its last state write is two
    /// days old — the clock must never read it.
    fn fenced_worker(fenced_ago: i64) -> (Value, AgentProbe) {
        let a = json!({
            "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
            "state": "attention", "updated": (NOW - 2 * 86_400) as f64,
            "params": {"upstream": "pm"}, "dead": false,
        });
        let probe = AgentProbe {
            show: Some(json!({"messages": [
                {"id": "m0", "state": "completed", "completed": (NOW - 3 * 86_400) as f64},
                {"id": "m1", "state": "unknown", "completed": (NOW - fenced_ago) as f64},
            ]})),
            ..AgentProbe::default()
        };
        (a, probe)
    }

    fn pm_row(dead: bool, state: &str) -> Value {
        json!({"alias": "pm", "provider": "devin", "endpoint_kind": "pty",
               "state": state, "dead": dead})
    }

    /// Classify then merge, the way `overview_with` does.
    fn resolve(mut rows: Vec<Item>, agents: &[Value]) -> Vec<Value> {
        classify_needs(
            &mut rows,
            &Owners::new(true, agents),
            NOW,
            ESCALATE_AFTER_SECS,
        );
        merge_by_subject(rows).into_iter().map(|i| i.json).collect()
    }

    fn fenced_rows(fenced_ago: i64, pm: Option<Value>) -> Vec<Value> {
        let (w1, probe) = fenced_worker(fenced_ago);
        let mut agents = vec![w1.clone()];
        agents.extend(pm);
        resolve(agent_items(&w1, &probe, "cadence", NOW), &agents)
    }

    /// CAD-374: only the operator may unfence or reconcile, so a fenced
    /// agent's row is the operator's from the start — live PM or dead.
    #[test]
    fn fenced_agent_goes_to_the_operator_whatever_its_pm() {
        for (pm, ago) in [
            (pm_row(true, "idle"), 120),
            (pm_row(false, "idle"), 10 * 60),
        ] {
            let out = fenced_rows(ago, Some(pm));
            assert_eq!(out.len(), 1);
            assert_eq!(out[0]["kind"], "fenced");
            assert_eq!(out[0]["since"], NOW - ago, "{}", out[0]);
            assert_eq!(out[0]["audience"], "operator", "{}", out[0]);
            assert_eq!(out[0]["audience_reason"], "operator decision");
        }
    }

    /// CAD-413: a failed auto-resume is one row naming the agent and
    /// the waiting message, clocked from the failure — it replaces the
    /// generic `fenced` row the failed open left in `attention`.
    #[test]
    fn failed_auto_resume_names_agent_and_waiting_message() {
        let (mut w1, probe) = fenced_worker(120);
        w1["auto_resume_failed"] = json!({
            "at": (NOW - 300) as f64, "message": "m-wait", "queued": 1,
            "reason": "fake open refused", "resume": "cadence agent resume w1",
        });
        let pm = pm_row(false, "idle");
        let out = resolve(agent_items(&w1, &probe, "cadence", NOW), &[w1.clone(), pm]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["kind"], "auto_resume_failed", "{}", out[0]);
        let title = out[0]["title"].as_str().unwrap();
        assert!(title.contains("agent w1"), "{title}");
        assert!(title.contains("message m-wait waiting"), "{title}");
        assert!(title.contains("fake open refused"), "{title}");
        assert_eq!(out[0]["command"], "cadence agent resume w1");
        assert_eq!(out[0]["since"], NOW - 300, "{}", out[0]);
    }

    /// The clock is when the issue entered its status, not its age: an
    /// issue created 30 days ago that went to review 10 minutes ago is
    /// fresh team work; 74 minutes in review escalates.
    #[test]
    fn tracker_row_clock_is_its_status_change_not_the_issue_age() {
        let pm = pm_row(false, "idle");
        let review = |in_status: i64| {
            item(
                70,
                "review_no_pr",
                "CAD-1 in review",
                30 * 86_400,
                "",
                None,
                "c",
            )
            .about("issue", "CAD-1")
            .owned_by(Some("pm"))
            .since(Some(NOW - in_status))
        };
        let out = resolve(vec![review(10 * 60)], std::slice::from_ref(&pm));
        assert_eq!(out[0]["audience"], "team", "{}", out[0]);
        assert_eq!(out[0]["audience_reason"], "owner pm can act");
        let out = resolve(vec![review(74 * 60)], &[pm]);
        assert_eq!(out[0]["audience"], "operator", "{}", out[0]);
        assert_eq!(out[0]["audience_reason"], "unhandled 74m");
    }

    /// A row with no reliable start time never escalates by age, however
    /// old its subject, and never claims "unhandled".
    #[test]
    fn row_without_a_start_time_never_escalates_by_age() {
        let pm = pm_row(false, "idle");
        let old = 30 * 86_400;
        // An approval menu is a pane sample with no start, even on a
        // days-old agent row.
        let a = json!({
            "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
            "state": "busy", "updated": (NOW - old) as f64,
            "params": {"upstream": "pm"}, "pane_menu": "1. Yes",
        });
        let mut rows = agent_items(&a, &AgentProbe::default(), "", NOW);
        // A tracker row whose status has no single change (a rollup),
        // and a PR head with no checks yet.
        rows.push(
            item(70, "review_no_pr", "CAD-9", old, "", None, "c")
                .about("issue", "CAD-9")
                .owned_by(Some("pm")),
        );
        rows.push(
            item(60, "pr_no_verdict", "PR #3", old, "", None, "c")
                .about("pr", "a/b#3")
                .owned_by(Some("pm"))
                .since(rollup_span(&[]).0),
        );
        let out = resolve(rows, &[a.clone(), pm]);
        assert_eq!(out.len(), 3);
        for row in &out {
            assert_eq!(row["since"], Value::Null, "{row}");
            assert_eq!(row["audience"], "team", "{row}");
            assert_eq!(row["audience_reason"], "owner pm can act", "{row}");
        }
    }

    /// A fence with no unknown turn (a disconnect while idle) is timed
    /// from the row's last write — a lower bound on time in `attention`.
    #[test]
    fn fence_without_an_unknown_turn_is_timed_from_its_last_write() {
        let pm = pm_row(false, "idle");
        let fenced = |written_ago: i64| {
            let a = json!({
                "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
                "state": "attention", "updated": (NOW - written_ago) as f64,
                "params": {"upstream": "pm"},
                "error": "Provider process disconnected while idle",
            });
            let probe = AgentProbe {
                show: Some(json!({"messages": [
                    {"id": "m0", "state": "completed", "completed": (NOW - 9 * 3_600) as f64},
                ]})),
                ..AgentProbe::default()
            };
            resolve(agent_items(&a, &probe, "", NOW), &[a.clone(), pm.clone()])
        };
        let out = fenced(74 * 60);
        assert_eq!(out[0]["since"], NOW - 74 * 60, "{}", out[0]);
        // The operator's from the start (CAD-374).
        assert_eq!(out[0]["audience_reason"], "operator decision");
        // A later params write shortens the clock; it never inflates it.
        assert_eq!(fenced(10 * 60)[0]["since"], NOW - 10 * 60);
    }

    #[test]
    fn owner_that_cannot_act_escalates_with_the_reason() {
        // A team row the PM owns (a fenced row is the operator's alone,
        // CAD-374, so it cannot show the owner's reason).
        let owners_of = |pm: Option<Value>| {
            let agents: Vec<Value> = pm.into_iter().collect();
            let mut rows = vec![item(40, "stalled", "t", 60, "", None, "c").owned_by(Some("pm"))];
            classify_needs(
                &mut rows,
                &Owners::new(true, &agents),
                NOW,
                ESCALATE_AFTER_SECS,
            );
            (
                rows[0].json["audience"].clone(),
                rows[0].json["audience_reason"].clone(),
            )
        };
        assert_eq!(
            owners_of(Some(pm_row(false, "attention"))),
            (json!("operator"), json!("owner pm is fenced"))
        );
        assert_eq!(
            owners_of(Some(pm_row(false, "stopped"))),
            (json!("operator"), json!("owner pm is stopped"))
        );
        assert_eq!(
            owners_of(None),
            (json!("operator"), json!("owner pm is absent"))
        );
        let drained = json!({"alias": "pm", "provider": "inbox", "state": "idle",
                             "dead": false, "inbox_health": {"stale": true}});
        assert_eq!(
            owners_of(Some(drained)),
            (json!("operator"), json!("owner pm has no inbox consumer"))
        );
        // An unreachable daemon cannot vouch for any owner.
        let mut rows = vec![item(40, "stalled", "t", 1, "", None, "c").owned_by(Some("pm"))];
        classify_needs(
            &mut rows,
            &Owners::new(false, &[]),
            NOW,
            ESCALATE_AFTER_SECS,
        );
        assert_eq!(rows[0].json["audience"], "operator");
    }

    #[test]
    fn merge_ready_pr_with_live_issue_owner_stays_team() {
        let owner = json!({"alias": "w9", "provider": "devin", "state": "busy", "dead": false});
        // Merge-ready since the verdict landed 5 minutes ago.
        let rollup = [
            json!({"__typename": "CheckRun", "startedAt": "1970-01-12T13:16:40Z",
                   "completedAt": "1970-01-12T13:40:00Z"}),
            json!({"__typename": "StatusContext", "context": "qa-verdict",
                   "startedAt": "1970-01-12T13:41:40Z"}),
            json!({"__typename": "CheckRun", "startedAt": "0001-01-01T00:00:00Z",
                   "completedAt": "0001-01-01T00:00:00Z"}),
        ];
        let (first, last) = rollup_span(&rollup);
        assert_eq!((first, last), (Some(NOW - 1_800), Some(NOW - 300)));
        let pr = item(
            10,
            "merge",
            "PR #7 fix — verdict pass",
            3_600,
            "cadence",
            None,
            "c",
        )
        .about("pr", "acme/widgets#7")
        .for_agent("w9")
        .owned_by(Some("w9"))
        .since(last);
        let out = resolve(vec![pr], &[owner]);
        assert_eq!(out[0]["audience"], "team", "{}", out[0]);
        assert_eq!(out[0]["audience_reason"], "owner w9 can act");
    }

    #[test]
    fn row_without_a_resolvable_owner_is_the_operators() {
        let out = resolve(
            vec![
                item(90, "ci_red", "main CI failed", 60, "", None, "c").about("ci", "a/b@main"),
                // An issue with no owner (`owned_by` drops the empty one).
                item(70, "review_no_pr", "CAD-1", 60, "", None, "c")
                    .about("issue", "CAD-1")
                    .owned_by(Some("")),
                // A root agent — no upstream PM.
                item(40, "stalled", "w2", 60, "", None, "c").about("agent", "w2"),
            ],
            &[],
        );
        for row in &out {
            assert_eq!(row["audience"], "operator", "{row}");
            assert_eq!(row["audience_reason"], "no owner", "{row}");
        }
    }

    #[test]
    fn kind_class_holds_outside_team_rows() {
        let old = Some(NOW - ESCALATE_AFTER_SECS * 10);
        let out = resolve(
            vec![
                item(20, "approval", "a", 1, "", None, "c").about("agent", "w1"),
                item(50, "drift", "d", 1, "", None, "c")
                    .about("deploy", "x")
                    .since(old),
                item(100, "inbox_unread", "i", 1, "", None, "c")
                    .about("agent", "w2")
                    .since(old),
                item(110, "tracker_behind", "t", 1, "", None, "c")
                    .about("tracker", "pm")
                    .since(old),
            ],
            &[],
        );
        let got: Vec<(&str, &Value)> = out
            .iter()
            .map(|r| (r["audience"].as_str().unwrap(), &r["audience_reason"]))
            .collect();
        assert_eq!(
            got,
            [
                ("operator", &json!("operator decision")),
                ("dependency", &Value::Null),
                ("info", &Value::Null),
                ("info", &Value::Null),
            ]
        );
    }

    /// A merged row is for whoever its most urgent cause is for: a
    /// fresh approval menu (team) plus an old stall (escalated) on one
    /// agent is the operator's, and each cause keeps its own audience.
    #[test]
    fn merged_row_takes_its_most_urgent_audience() {
        let pm = pm_row(false, "idle");
        let a = json!({
            "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
            "state": "busy", "updated": (NOW - 60) as f64,
            "params": {"upstream": "pm"}, "pane_menu": "1. Yes",
            "stalled": true, "silent_secs": ESCALATE_AFTER_SECS + 60,
        });
        let out = resolve(agent_items(&a, &AgentProbe::default(), "", NOW), &[pm]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["kind"], "approval_menu", "primary stays by rank");
        assert_eq!(out[0]["audience"], "operator");
        assert_eq!(out[0]["audience_reason"], "unhandled 61m");
        assert_eq!(out[0]["causes"][0]["audience"], "team");
        assert_eq!(out[0]["causes"][1]["audience"], "operator");
        assert_eq!(out[0]["causes"][1]["since"], NOW - ESCALATE_AFTER_SECS - 60);
    }

    #[test]
    fn scope_keeps_project_and_group_rows() {
        let rows = || {
            vec![
                item(40, "stalled", "a", 1, "cadence", None, "c")
                    .about("agent", "w1")
                    .for_agent("w1"),
                item(40, "stalled", "b", 1, "other", None, "c")
                    .about("agent", "w2")
                    .for_agent("w2"),
                item(90, "ci_red", "c", 1, "cadence", None, "c").about("repo", "a/b"),
            ]
        };
        let titles = |v: Vec<Item>| -> Vec<String> {
            v.iter()
                .map(|i| i.json["title"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            titles(scope_rows(rows(), Some("cadence"), None)),
            ["a", "c"]
        );
        let members = vec!["pm".to_string(), "w2".to_string()];
        assert_eq!(titles(scope_rows(rows(), None, Some(&members))), ["b"]);
        assert_eq!(titles(scope_rows(rows(), None, None)).len(), 3);
    }

    #[test]
    fn agent_project_is_longest_repo_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let wt = root.join(".cadence/wt/x");
        std::fs::create_dir_all(&wt).unwrap();
        let repos = vec![
            (root.clone(), "outer".to_string()),
            (root.join(".cadence"), "inner".to_string()),
        ];
        let a = json!({"cwd": wt.to_str().unwrap()});
        assert_eq!(agent_project(&a, &repos), "inner");
        assert_eq!(agent_project(&json!({"cwd": "/elsewhere"}), &repos), "");
        assert_eq!(agent_project(&json!({}), &repos), "");
    }

    // ---- CAD-267: default-branch CI from Actions runs ----

    /// A fake 40-hex SHA from a digit — built at runtime, never a
    /// credential-shaped literal.
    fn sha(n: u8) -> String {
        format!("{n}").repeat(40)
    }

    /// One workflow run the way the runs API lists it. `created` orders
    /// runs; `id` rises with it.
    fn run(workflow: &str, sha_n: u8, id: u64, status: &str, conclusion: Option<&str>) -> Value {
        json!({
            "id": id, "head_sha": sha(sha_n), "status": status,
            "conclusion": conclusion, "event": "push",
            "path": format!(".github/workflows/{workflow}"), "head_branch": "main",
            "html_url": format!("https://github.com/o/r/actions/runs/{id}"),
            "created_at": format!("2026-09-23T01:{:02}:00Z", id % 60),
        })
    }

    fn ci(sha_n: u8, id: u64, status: &str, conclusion: Option<&str>) -> Value {
        run("ci.yml", sha_n, id, status, conclusion)
    }

    fn states(shas: &[ShaCi]) -> Vec<(&str, CiState, Option<&str>)> {
        shas.iter()
            .map(|s| {
                (
                    &s.sha[..1],
                    s.state,
                    s.covered_by.as_deref().map(|c| &c[..1]),
                )
            })
            .collect()
    }

    /// First-parent log for SHAs 1 (oldest) … n (newest), newest first.
    fn log(n: u8) -> Vec<String> {
        (1..=n).rev().map(sha).collect()
    }

    /// Three rapid pushes: 1 passed, 2's run cancelled, 3 still pending.
    /// 2 has no covering descendant yet → ci_unverified; pending never
    /// alerts and nothing is red.
    #[test]
    fn main_ci_middle_cancelled_newest_pending_is_unverified() {
        let runs = [
            ci(3, 30, "in_progress", None),
            ci(2, 20, "completed", Some("cancelled")),
            ci(1, 10, "completed", Some("success")),
        ];
        let shas = classify_main_ci(&runs, &log(3));
        use CiState::*;
        assert_eq!(
            states(&shas),
            [
                ("3", Pending, None),
                ("2", Cancelled, None),
                ("1", Passed, None)
            ]
        );
        let a = main_ci_alerts(&shas);
        assert!(a.red.is_none(), "pending never alerts");
        assert_eq!(a.unverified.len(), 1);
        assert_eq!(a.unverified[0].sha, sha(2));
    }

    /// Newest failed: ci_red on it, and the cancelled middle stays
    /// uncovered — a failed descendant covers nothing.
    #[test]
    fn main_ci_newest_failed_is_red_and_middle_uncovered() {
        use CiState::*;
        for bad in ["failure", "timed_out", "startup_failure"] {
            let runs = [
                ci(3, 30, "completed", Some(bad)),
                ci(2, 20, "completed", Some("cancelled")),
                ci(1, 10, "completed", Some("success")),
            ];
            let shas = classify_main_ci(&runs, &log(3));
            assert_eq!(
                states(&shas),
                [
                    ("3", Failed, None),
                    ("2", Cancelled, None),
                    ("1", Passed, None)
                ],
                "{bad}"
            );
            let a = main_ci_alerts(&shas);
            assert_eq!(a.red.map(|s| s.sha.clone()), Some(sha(3)), "{bad}");
            assert_eq!(a.unverified.len(), 1, "{bad}");
            assert_eq!(a.unverified[0].sha, sha(2));
        }
    }

    /// Newest passed: the cancelled middle is covered by it — and still
    /// labelled cancelled, never passed. No alert.
    #[test]
    fn main_ci_newest_passed_covers_middle_without_passing_it() {
        let runs = [
            ci(3, 30, "completed", Some("success")),
            ci(2, 20, "completed", Some("cancelled")),
            ci(1, 10, "completed", Some("success")),
        ];
        let shas = classify_main_ci(&runs, &log(3));
        use CiState::*;
        assert_eq!(
            states(&shas),
            [
                ("3", Passed, None),
                ("2", Cancelled, Some("3")),
                ("1", Passed, None)
            ]
        );
        assert_ne!(shas[1].state, Passed);
        let a = main_ci_alerts(&shas);
        assert!(a.red.is_none());
        assert!(a.unverified.is_empty(), "covered clears the alert");
    }

    /// A SHA whose only runs are another workflow's (Handover) — even a
    /// passing one — is missing, never passed; a non-push ci run does
    /// not count either.
    #[test]
    fn main_ci_handover_only_sha_is_missing_not_passed() {
        let mut dispatch = ci(2, 21, "completed", Some("success"));
        dispatch["event"] = json!("workflow_dispatch");
        let runs = [
            run("handover.yml", 2, 22, "completed", Some("success")),
            dispatch,
            ci(1, 10, "completed", Some("success")),
        ];
        let shas = classify_main_ci(&runs, &log(2));
        use CiState::*;
        assert_eq!(states(&shas), [("2", Missing, None), ("1", Passed, None)]);
        assert!(shas[0].run_id.is_none());
        let a = main_ci_alerts(&shas);
        assert_eq!(a.unverified.len(), 1);
        assert_eq!(a.unverified[0].state, Missing);
        // A later passing SHA covers the missing one; it stays missing.
        let runs = [ci(3, 30, "completed", Some("success")), runs[0].clone()];
        let shas = classify_main_ci(&runs, &log(3));
        assert_eq!(shas[1].state, Missing);
        assert_eq!(shas[1].covered_by, Some(sha(3)));
    }

    /// Run SHAs newer than the clone's last fetch lead the list; an
    /// unlisted SHA older than every listed run is not first-parent
    /// history and is dropped. With no clone, runs alone give the order.
    #[test]
    fn main_ci_places_runs_the_clone_has_not_fetched() {
        let runs = [
            ci(4, 40, "completed", Some("success")),
            ci(2, 20, "completed", Some("cancelled")),
            ci(1, 10, "completed", Some("success")),
            // Off first-parent history (older than every listed run).
            ci(9, 5, "completed", Some("success")),
        ];
        use CiState::*;
        // The clone knows 1..2 only; 4 was pushed after its fetch.
        let shas = classify_main_ci(&runs, &log(2));
        assert_eq!(
            states(&shas),
            [
                ("4", Passed, None),
                ("2", Cancelled, Some("4")),
                ("1", Passed, None)
            ]
        );
        // No clone: every run SHA, newest run first.
        let shas = classify_main_ci(&runs, &[]);
        assert_eq!(
            shas.iter().map(|s| &s.sha[..1]).collect::<Vec<_>>(),
            ["4", "2", "1", "9"]
        );
        // Per SHA the newest run decides (a re-push of the same SHA).
        let runs = [
            ci(1, 11, "completed", Some("failure")),
            ci(1, 10, "completed", Some("success")),
        ];
        assert_eq!(classify_main_ci(&runs, &log(1))[0].state, Failed);
    }

    /// The needs-me rows: ci_red and ci_unverified share the branch's
    /// subject (one row, two causes); titles end in the slug, which
    /// `session`'s ack key reads.
    #[test]
    fn main_ci_rows_share_the_branch_subject() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        let mut shas = Vec::new();
        for i in 0..3 {
            std::fs::write(repo.join("f"), format!("{i}")).unwrap();
            git(repo, &["add", "f"]);
            git(repo, &["commit", "-qm", &format!("c{i}")]);
            shas.push(
                git_text(repo, &["rev-parse".into(), "HEAD".into()])
                    .unwrap()
                    .trim()
                    .to_string(),
            );
        }
        let with_sha = |mut r: Value, i: usize| {
            r["head_sha"] = json!(shas[i]);
            r
        };
        let data = json!({"main_ci": {"branch": "main", "runs": [
            with_sha(ci(0, 30, "completed", Some("failure")), 2),
            with_sha(ci(0, 20, "completed", Some("cancelled")), 1),
            with_sha(ci(0, 10, "completed", Some("success")), 0),
        ]}});
        let (view, rows) = main_ci_view("o/r", "cadence", &data, Some(repo), 0);
        assert_eq!(view["order"], "first_parent", "{view}");
        let got: Vec<&str> = view["shas"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["state"].as_str().unwrap())
            .collect();
        assert_eq!(got, ["failed", "cancelled", "passed"], "{view}");
        let merged = merge_by_subject(rows);
        assert_eq!(merged.len(), 1);
        let row = &merged[0].json;
        assert_eq!(row["subject"], json!({"kind": "ci", "id": "o/r@main"}));
        assert_eq!(row["kind"], "ci_red");
        assert_eq!(row["command"], "gh run view 30 --repo o/r");
        let causes: Vec<&str> = row["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["cause"].as_str().unwrap())
            .collect();
        assert_eq!(causes, ["ci_red", "ci_unverified"]);
        assert_eq!(row["causes"][1]["command"], "gh run rerun 20 --repo o/r");
        assert!(row["title"].as_str().unwrap().ends_with(" o/r"), "{row}");
        assert!(row["causes"][1]["title"]
            .as_str()
            .unwrap()
            .contains(&shas[1][..7]));
        // No ci.yml workflow: nothing to show, nothing to alert.
        let absent = json!({"main_ci": {"branch": "main", "absent": true}});
        let (view, rows) = main_ci_view("o/r", "cadence", &absent, Some(repo), 0);
        assert!(view.is_null() && rows.is_empty());
        // A failed fetch surfaces its error, never a verdict.
        let failed = json!({"main_ci": {"branch": "main", "error": "gh: boom"}});
        let (view, rows) = main_ci_view("o/r", "cadence", &failed, Some(repo), 0);
        assert_eq!(view["error"], "gh: boom");
        assert!(rows.is_empty());
    }

    fn slow_gh(_slug: &str) -> Result<Value, String> {
        std::thread::sleep(Duration::from_secs(3));
        Ok(json!({"prs": [{"number": 2}], "ci": {"state": "success"}}))
    }

    /// CAD-249: a gh refresh slower than the caller's wait serves the
    /// last cache as `stale` with its `as_of` inside the bound, and the
    /// refresh still lands in the cache for the next request.
    #[test]
    fn slow_gh_serves_stale_cache_within_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let slugs = vec!["acme/widgets".to_string()];
        let old = now_epoch() - 600;
        let mut repos = HashMap::new();
        repos.insert(
            "acme/widgets".to_string(),
            json!({"prs": [{"number": 1}], "ci": {}}),
        );
        write_cache(&cache_file(dir.path()), &slugs, &repos, old);

        let started = Instant::now();
        let (got, state) = github_bounded(dir.path(), &slugs, Duration::from_millis(300), slow_gh);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(state["state"], "stale", "{state}");
        assert_eq!(state["as_of"], old, "{state}");
        assert!(
            state["error"].as_str().unwrap().contains("still running"),
            "{state}"
        );
        assert_eq!(got["acme/widgets"]["prs"][0]["number"], 1);

        // A second request while the refresh runs starts no other one.
        let (_, again) = github_bounded(dir.path(), &slugs, Duration::from_millis(100), slow_gh);
        assert_eq!(again["state"], "stale", "{again}");

        // The background refresh lands; the next request is a cache hit.
        let deadline = Instant::now() + Duration::from_secs(15);
        while read_cache(&cache_file(dir.path())).is_none_or(|c| c.at == old) {
            assert!(Instant::now() < deadline, "refresh never landed");
            std::thread::sleep(Duration::from_millis(50));
        }
        let (got, state) = github_bounded(dir.path(), &slugs, Duration::from_millis(1), slow_gh);
        assert_eq!(state["state"], "cached", "{state}");
        assert_eq!(got["acme/widgets"]["prs"][0]["number"], 2);
    }

    /// No cache at all and a slow gh: `unavailable` inside the bound,
    /// never a hang.
    #[test]
    fn slow_gh_without_cache_is_unavailable_not_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let slugs = vec!["acme/gadgets".to_string()];
        let started = Instant::now();
        let (got, state) = github_bounded(dir.path(), &slugs, Duration::from_millis(200), slow_gh);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(got.is_empty());
        assert_eq!(state["state"], "unavailable", "{state}");
        assert!(state["as_of"].is_null());
    }

    /// A daemon that answers `health`/`agent_list` but never answers
    /// the per-agent probes: the overview still returns inside its
    /// bounds, naming what timed out in `degraded`.
    #[test]
    fn wedged_agent_probes_degrade_within_the_bound() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(client::socket_path(dir.path())).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut line = String::new();
                    let mut reader = BufReader::new(&stream);
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap_or_default();
                    let result = match req["method"].as_str().unwrap_or_default() {
                        "health" => json!({"state": "ready"}),
                        "daemon_info" => json!({"build_commit": "unknown"}),
                        "agent_list" => json!({"agents": [{
                            "alias": "w1", "provider": "claude",
                            "endpoint_kind": "managed", "state": "busy",
                            "updated": 0.0,
                        }]}),
                        // agent_show / agent_requests / monitors: wedged.
                        _ => {
                            std::thread::sleep(Duration::from_secs(30));
                            return;
                        }
                    };
                    let frame = json!({"ok": true, "result": result});
                    let _ = writeln!(&stream, "{frame}");
                });
            }
        });
        let opts = Options {
            probe_timeout: Duration::from_millis(300),
            probe_budget: Duration::from_millis(800),
            ..Options::board()
        };
        let started = Instant::now();
        let view = overview_with(dir.path(), &dir.path().join("no-pm"), &opts).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(view["daemon"]["reachable"], true, "{view}");
        let notes = view["degraded"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|d| d["source"] == "agent_show" && d["subject"] == "w1"),
            "{view}"
        );
        assert!(
            notes[0]["detail"]
                .as_str()
                .unwrap()
                .contains("no answer within"),
            "{view}"
        );
    }

    #[test]
    fn monitoring_projection_keeps_real_scan_and_actionable_alert_evidence() {
        let monitors = vec![json!({
            "id": "watch-cadence",
            "project": "cadence",
            "owner": "watchdog",
            "monitoring": "degraded",
            "last_success_at": 90.0,
            "last_check_at": 110.0,
            "next_check_at": 170.0,
            "delivery": {"configured": false, "state": "unconfigured"},
            "error": "socket closed",
        })];
        let mut alerts_by_monitor = HashMap::new();
        alerts_by_monitor.insert(
            "watch-cadence".to_string(),
            vec![json!({
                "seq": 7,
                "monitor": "watch-cadence",
                "task": "cad-176-t1",
                "event_seq": 44,
                "fingerprint": "turn-stalled:44",
                "kind": "turn_stalled",
                "payload": {"message": "synthetic evidence"},
                "state": "open",
                "attempts": 1,
                "last_error": Value::Null,
                "created": 100.0,
                "updated": 100.0,
            })],
        );

        let view = monitoring_view(monitors, alerts_by_monitor, HashMap::new(), 130);
        assert_eq!(view["state"], "degraded");
        assert_eq!(view["last_success_at"], 90);
        assert_eq!(view["last_check_at"], 110);
        assert_eq!(view["open_alerts"], 1);
        assert_eq!(view["delivery"]["configured"], false);
        assert_eq!(view["delivery"]["push"], false);
        let alert = &view["alerts"][0];
        assert_eq!(alert["age_secs"], 30);
        assert_eq!(alert["next_owner"], "watchdog");
        assert_eq!(alert["authority"], "monitor owner may reconcile the worker");
        assert_eq!(alert["evidence"]["event_seq"], 44);
        assert_eq!(alert["evidence"]["fingerprint"], "turn-stalled:44");
    }

    #[test]
    fn turn_unknown_overview_requires_operator_reconciliation() {
        let monitors = vec![json!({
            "id": "unknown-monitor",
            "project": "cadence",
            "owner": "operator",
            "monitoring": "active",
            "last_success_at": 90.0,
            "last_check_at": 100.0,
            "next_check_at": 160.0,
            "error": Value::Null,
        })];
        let mut alerts_by_monitor = HashMap::new();
        alerts_by_monitor.insert(
            "unknown-monitor".to_string(),
            vec![json!({
                "seq": 3,
                "monitor": "unknown-monitor",
                "task": "unknown-task",
                "event_seq": 9,
                "fingerprint": "event:9",
                "kind": "turn_unknown",
                "payload": {"reason": "bounded"},
                "state": "open",
                "attempts": 0,
                "last_error": Value::Null,
                "created": 100.0,
                "updated": 100.0,
            })],
        );
        let view = monitoring_view(monitors, alerts_by_monitor, HashMap::new(), 130);
        let alert = &view["alerts"][0];
        assert_eq!(alert["kind"], "turn_unknown");
        assert_eq!(alert["next_owner"], "operator");
        assert_eq!(alert["authority"], "operator reconciliation required");
        let action = alert["next_action"].as_str().unwrap();
        assert!(action.contains("Inspect"), "{action}");
        assert!(action.contains("reconcil"), "{action}");
    }

    #[test]
    fn monitoring_projection_reports_stopped_without_registration() {
        let view = monitoring_view(Vec::new(), HashMap::new(), HashMap::new(), 130);
        assert_eq!(view["available"], true);
        assert_eq!(view["state"], "stopped");
        assert_eq!(view["last_success_at"], Value::Null);
        assert!(view["alerts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn monitoring_projection_marks_overdue_active_scan_stale() {
        let view = monitoring_view(
            vec![json!({
                "id": "watch",
                "project": "cadence",
                "owner": "watchdog",
                "monitoring": "active",
                "last_success_at": 90.0,
                "last_check_at": 90.0,
                "next_check_at": 100.0,
                "error": Value::Null,
            })],
            HashMap::new(),
            HashMap::new(),
            101,
        );
        assert_eq!(view["state"], "stale");
        assert_eq!(view["monitors"][0]["monitoring"], "stale");
        assert_eq!(view["monitors"][0]["stale"], true);
        assert_eq!(
            view["errors"][0]["error"],
            "scheduled monitor reconciliation is overdue"
        );
    }

    #[test]
    fn monitoring_projection_keeps_stale_aggregate_when_active_row_follows() {
        let overdue = json!({
            "id": "overdue",
            "project": "cadence",
            "owner": "watchdog",
            "monitoring": "active",
            "last_success_at": 90.0,
            "last_check_at": 90.0,
            "next_check_at": 100.0,
            "error": Value::Null,
        });
        let current = json!({
            "id": "current",
            "project": "cadence",
            "owner": "watchdog",
            "monitoring": "active",
            "last_success_at": 110.0,
            "last_check_at": 110.0,
            "next_check_at": 200.0,
            "error": Value::Null,
        });
        let forward = monitoring_view(
            vec![overdue.clone(), current.clone()],
            HashMap::new(),
            HashMap::new(),
            101,
        );
        let reverse = monitoring_view(vec![current, overdue], HashMap::new(), HashMap::new(), 101);
        assert_eq!(forward["state"], "stale", "{forward}");
        assert_eq!(reverse["state"], "stale", "{reverse}");
    }

    #[test]
    fn malformed_monitor_rows_fail_closed_before_projection() {
        assert!(validate_monitor_rows(&[]).is_ok());
        let error = validate_monitor_rows(&[json!({"project": "cadence"})]).unwrap_err();
        assert!(error.contains("row 0") && error.contains("id"), "{error}");
        let unavailable = monitoring_unavailable(error);
        assert_eq!(unavailable["available"], false);
        assert_eq!(unavailable["state"], "unavailable");
        assert!(unavailable["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("row 0"));
        let error = validate_monitor_rows(&[json!({"id": "watch"})]).unwrap_err();
        assert!(
            error.contains("row 0") && error.contains("project"),
            "{error}"
        );
        let error = validate_monitor_rows(&[json!({
            "id": "watch",
            "project": "cadence",
            "owner": "watchdog",
            "monitoring": "bogus",
            "interval_secs": 60,
            "coverage": [],
            "delivery": {"configured": false, "state": "unconfigured"},
            "open_alerts": 0,
            "total_alerts": 0,
        })])
        .unwrap_err();
        assert!(error.contains("unsupported monitoring state"), "{error}");
        let unavailable = monitoring_unavailable(error);
        assert_eq!(unavailable["available"], false);
        assert_eq!(unavailable["state"], "unavailable");
    }

    #[test]
    fn monitor_alert_response_validation_accepts_empty_and_rejects_partial() {
        let empty = json!({"monitor": "watch", "alerts": [], "cursor": 0});
        assert!(validate_monitor_alert_response("watch", &empty)
            .unwrap()
            .is_empty());

        let missing_alerts = json!({"monitor": "watch", "cursor": 0});
        let error = validate_monitor_alert_response("watch", &missing_alerts).unwrap_err();
        assert!(error.contains("missing alerts"), "{error}");
        let unavailable = monitoring_unavailable(format!("monitor_alerts 'watch': {error}"));
        assert_eq!(unavailable["available"], false);

        let valid_alert = json!({
            "seq": 1,
            "monitor": "watch",
            "task": "task-1",
            "event_seq": 2,
            "fingerprint": "event:2",
            "kind": "turn_stalled",
            "payload": {"message": "synthetic"},
            "state": "open",
            "attempts": 0,
            "last_error": Value::Null,
            "created": 10.0,
            "updated": 10.0,
        });
        let response = json!({"monitor": "watch", "alerts": [valid_alert], "cursor": 1});
        assert_eq!(
            validate_monitor_alert_response("watch", &response)
                .unwrap()
                .len(),
            1
        );

        let malformed_alert = json!({
            "seq": 1,
            "monitor": "watch",
            "task": "task-1",
            "event_seq": 2,
            "fingerprint": "event:2",
            "kind": "turn_stalled",
            "state": "open",
            "attempts": 0,
            "last_error": Value::Null,
            "created": 10.0,
            "updated": 10.0,
        });
        let response = json!({
            "monitor": "watch",
            "alerts": [malformed_alert],
            "cursor": 1,
        });
        let error = validate_monitor_alert_response("watch", &response).unwrap_err();
        assert!(error.contains("missing payload"), "{error}");

        let wrong_monitor = json!({"monitor": "other", "alerts": [], "cursor": 0});
        let error = validate_monitor_alert_response("watch", &wrong_monitor).unwrap_err();
        assert!(error.contains("wrong monitor"), "{error}");
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    }

    #[test]
    fn drift_counts_commits_and_prs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-b", "main"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        let commit = |msg: &str| {
            std::fs::write(repo.join("f"), msg).unwrap();
            git(repo, &["add", "f"]);
            git(repo, &["commit", "-m", msg]);
            git_text(repo, &["rev-parse".into(), "HEAD".into()])
                .unwrap()
                .trim()
                .to_string()
        };
        let base = commit("one (#1)");
        // Zero drift: building at HEAD.
        let d = compute_drift(repo, &base);
        assert_eq!(d["known"], true);
        assert_eq!(d["count"], 0);
        commit("two (#2)");
        commit("three — no pr");
        commit("four (#4)");
        let d = compute_drift(repo, &base);
        assert_eq!(d["known"], true);
        assert_eq!(d["count"], 3);
        let prs: Vec<Option<u64>> = d["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["pr"].as_u64())
            .collect();
        assert_eq!(prs, vec![Some(4), None, Some(2)]);
        // Unknown build — cannot tell, not zero.
        let d = compute_drift(repo, "unknown");
        assert_eq!(d["known"], false);
        assert!(d["reason"].as_str().unwrap().contains("cannot tell"));
        // A commit git cannot place — also cannot tell.
        let d = compute_drift(repo, "deadbeef".repeat(5).as_str());
        assert_eq!(d["known"], false);
    }

    #[test]
    fn drift_matches_repo_by_remote_or_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("clone");
        std::fs::create_dir_all(&repo).unwrap();
        let p = project::Project {
            key: "cadence".into(),
            prefix: "CAD".into(),
            repos: vec![project::Repo {
                path: Some(repo.to_string_lossy().into()),
                remote: Some("git@github.com:favcrm/cadence.git".into()),
            }],
            components: vec![],
            tags: vec![],
            default_owner: None,
            build: None,
            memory: None,
        };
        // Path match against BUILD_ROOT (this crate's checkout) never
        // hits the temp clone — remote match does when remote differs…
        let m = build_repo_match(std::slice::from_ref(&p));
        // Build remote is github.com/favcrm/cadence in this checkout, so
        // the declared remote matches; on foreign checkouts (unknown
        // remote) nothing matches — either outcome is consistent.
        if BUILD_REMOTE != "unknown" {
            assert_eq!(m.map(|(k, _)| k), Some("cadence".to_string()));
        } else {
            assert!(m.is_none());
        }
        // A project whose remote differs and whose path differs never
        // matches.
        let other = project::Project {
            key: "other".into(),
            prefix: "OTH".into(),
            repos: vec![project::Repo {
                path: Some("/definitely/not/the/build".into()),
                remote: Some("git@example.com:other/repo.git".into()),
            }],
            components: vec![],
            tags: vec![],
            default_owner: None,
            build: None,
            memory: None,
        };
        assert!(build_repo_match(&[other]).is_none());
    }
}
