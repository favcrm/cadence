//! Routines, deterministic schedule/event runs, connection policies, and staged
//! assistant proposals (CAD-2317).
//!
//! Invariants enforced in store + scheduler helpers:
//! - Recurring schedules require an explicit IANA timezone (`chrono_tz::Tz`) and
//!   a minimum 15-minute cadence (`interval_mins >= 15` and cron gap `>= 15m`).
//! - `(routine_id, due_instant)` is durably unique: retries or overlapping wakeups
//!   for the same slot deduplicate instead of double-firing.
//! - Scheduled slots more than 10 minutes (`600s`) stale record `skipped_stale`
//!   and advance `next_due_at` past `now_epoch` (no offline catch-up burst).
//! - At most one `running` run per routine is active at a time (overlap guard).
//! - Ten consecutive failed runs (`>= 10`) auto-pause the routine.
//! - Missing or archived `destination_thread` transitions the routine to
//!   `repoint_required` (`Choose thread`) until repointed to a live thread.

use chrono::{Datelike, TimeZone, Timelike};
use chrono_tz::Tz;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{Error, Result};

use super::{now, Store, StoreConn};

pub const STALE_SLOT_SECS: i64 = 600;
pub const MIN_INTERVAL_MINS: i64 = 15;
pub const AUTO_PAUSE_FAILURES: i64 = 10;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS routines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    agent_alias TEXT NOT NULL,
    trigger_kind TEXT NOT NULL,
    cron TEXT,
    interval_mins INTEGER,
    timezone TEXT,
    event_source TEXT,
    target_kind TEXT NOT NULL,
    markdown_path TEXT,
    approved_rev TEXT,
    script_path TEXT,
    approved_sha256 TEXT,
    destination_thread TEXT,
    connection_account TEXT,
    checklist_json TEXT NOT NULL DEFAULT '[]',
    inputs_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL,
    pause_reason TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    next_due_at INTEGER,
    last_run_at INTEGER,
    last_run_state TEXT,
    created REAL NOT NULL,
    updated REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS routines_agent ON routines(agent_alias);
CREATE INDEX IF NOT EXISTS routines_due ON routines(status, next_due_at);

CREATE TABLE IF NOT EXISTS routine_runs (
    id TEXT PRIMARY KEY,
    routine_id TEXT NOT NULL,
    due_instant INTEGER NOT NULL,
    trigger_source TEXT NOT NULL,
    state TEXT NOT NULL,
    approved_rev TEXT,
    approved_sha256 TEXT,
    checklist_state_json TEXT NOT NULL DEFAULT '[]',
    script_output_json TEXT,
    message_id TEXT,
    error TEXT,
    started_at REAL NOT NULL,
    finished_at REAL,
    UNIQUE(routine_id, due_instant)
);
CREATE INDEX IF NOT EXISTS routine_runs_routine ON routine_runs(routine_id, started_at DESC);

CREATE TABLE IF NOT EXISTS connection_policies (
    connection_id TEXT PRIMARY KEY,
    provider TEXT NOT NULL DEFAULT '',
    visibility TEXT NOT NULL,
    owner_user_id TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,
    write_mode TEXT NOT NULL,
    allow_external_triggers INTEGER NOT NULL DEFAULT 0,
    allowed_agents_json TEXT NOT NULL DEFAULT '[]',
    policy_revision INTEGER NOT NULL DEFAULT 1,
    updated REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS connection_policies_provider ON connection_policies(provider, visibility);

CREATE TABLE IF NOT EXISTS staged_routine_proposals (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    proposed_by TEXT NOT NULL,
    state TEXT NOT NULL,
    title TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created REAL NOT NULL,
    updated REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS staged_routine_proposals_state ON staged_routine_proposals(state, created DESC);

CREATE TABLE IF NOT EXISTS credential_request_resolutions (
    request_id TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    conversation_id TEXT,
    resumed_at REAL NOT NULL
);
";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChecklistStep {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default = "default_step_state")]
    pub state: String,
}

fn default_step_state() -> String {
    "pending".to_string()
}

#[derive(Debug, Clone)]
pub struct Routine {
    pub id: String,
    pub name: String,
    pub agent_alias: String,
    pub trigger_kind: String,
    pub cron: Option<String>,
    pub interval_mins: Option<i64>,
    pub timezone: Option<String>,
    pub event_source: Option<String>,
    pub target_kind: String,
    pub markdown_path: Option<String>,
    pub approved_rev: Option<String>,
    pub script_path: Option<String>,
    pub approved_sha256: Option<String>,
    pub destination_thread: Option<String>,
    pub connection_account: Option<String>,
    pub checklist: Vec<ChecklistStep>,
    pub inputs: Value,
    pub status: String,
    pub pause_reason: Option<String>,
    pub consecutive_failures: i64,
    pub next_due_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub last_run_state: Option<String>,
    pub created: f64,
    pub updated: f64,
}

impl Routine {
    pub fn requires_markdown(&self) -> bool {
        matches!(
            self.target_kind.as_str(),
            "markdown" | "script_then_markdown"
        )
    }

    pub fn requires_script(&self) -> bool {
        matches!(self.target_kind.as_str(), "script" | "script_then_markdown")
    }

    pub fn to_json(
        &self,
        current_rev: Option<&str>,
        current_sha256: Option<&str>,
        thread_live: bool,
    ) -> Value {
        let rev_drift = self.requires_markdown()
            && match (&self.approved_rev, current_rev) {
                (Some(approved), Some(live)) => approved != live,
                _ => true,
            };
        let sha_drift = self.requires_script()
            && match (&self.approved_sha256, current_sha256) {
                (Some(approved), Some(live)) => approved != live,
                _ => true,
            };
        let approval_drift = rev_drift || sha_drift;
        let effective_status = if !thread_live && self.destination_thread.is_some() {
            "repoint_required"
        } else if self.status == "paused" {
            "paused"
        } else if approval_drift || self.status == "waiting_approval" {
            "waiting_approval"
        } else {
            self.status.as_str()
        };
        json!({
            "id": self.id,
            "name": self.name,
            "agent_alias": self.agent_alias,
            "trigger_kind": self.trigger_kind,
            "cron": self.cron,
            "interval_mins": self.interval_mins,
            "timezone": self.timezone,
            "event_source": self.event_source,
            "target_kind": self.target_kind,
            "markdown_path": self.markdown_path,
            "approved_rev": self.approved_rev,
            "current_rev": current_rev,
            "script_path": self.script_path,
            "approved_sha256": self.approved_sha256,
            "current_sha256": current_sha256,
            "approval_drift": approval_drift,
            "destination_thread": self.destination_thread,
            "destination_thread_live": thread_live,
            "connection_account": self.connection_account,
            "checklist": self.checklist,
            "inputs": self.inputs,
            "status": effective_status,
            "stored_status": self.status,
            "pause_reason": self.pause_reason,
            "consecutive_failures": self.consecutive_failures,
            "next_due_at": self.next_due_at,
            "last_run_at": self.last_run_at,
            "last_run_state": self.last_run_state,
            "schedule_summary": self.schedule_summary(),
            "created": self.created,
            "updated": self.updated,
        })
    }

    pub fn schedule_summary(&self) -> String {
        if self.trigger_kind == "event" {
            return format!(
                "When {}",
                self.event_source.as_deref().unwrap_or("event arrives")
            );
        }
        let tz = self.timezone.as_deref().unwrap_or("UTC");
        if let Some(cron) = self.cron.as_deref() {
            return format!("{cron} ({tz})");
        }
        if let Some(mins) = self.interval_mins {
            if mins % 1440 == 0 {
                let days = mins / 1440;
                if days == 1 {
                    return format!("Every day ({tz})");
                }
                return format!("Every {days} days ({tz})");
            }
            if mins % 60 == 0 {
                let hours = mins / 60;
                if hours == 1 {
                    return format!("Every hour ({tz})");
                }
                return format!("Every {hours} hours ({tz})");
            }
            return format!("Every {mins} min ({tz})");
        }
        format!("Scheduled ({tz})")
    }
}

#[derive(Debug, Clone)]
pub struct RoutineRun {
    pub id: String,
    pub routine_id: String,
    pub due_instant: i64,
    pub trigger_source: String,
    pub state: String,
    pub approved_rev: Option<String>,
    pub approved_sha256: Option<String>,
    pub checklist_state: Vec<ChecklistStep>,
    pub script_output: Option<Value>,
    pub message_id: Option<String>,
    pub error: Option<String>,
    pub started_at: f64,
    pub finished_at: Option<f64>,
}

impl RoutineRun {
    pub fn to_json(&self, deduped: bool) -> Value {
        json!({
            "id": self.id,
            "routine_id": self.routine_id,
            "due_instant": self.due_instant,
            "trigger_source": self.trigger_source,
            "state": self.state,
            "deduped": deduped,
            "approved_rev": self.approved_rev,
            "approved_sha256": self.approved_sha256,
            "checklist": self.checklist_state,
            "checklist_state": self.checklist_state,
            "script_output": self.script_output,
            "message_id": self.message_id,
            "error": self.error,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RoutineCreate {
    pub name: String,
    pub agent_alias: String,
    pub trigger_kind: String,
    pub cron: Option<String>,
    pub interval_mins: Option<i64>,
    pub timezone: Option<String>,
    pub event_source: Option<String>,
    pub target_kind: String,
    pub markdown_path: Option<String>,
    pub script_path: Option<String>,
    pub destination_thread: Option<String>,
    pub connection_account: Option<String>,
    pub checklist: Vec<ChecklistStep>,
    pub inputs: Value,
    pub now_epoch: i64,
}

#[derive(Debug, Clone)]
pub struct RoutineUpdate {
    pub name: Option<String>,
    pub trigger_kind: Option<String>,
    pub cron: Option<Option<String>>,
    pub interval_mins: Option<Option<i64>>,
    pub timezone: Option<String>,
    pub event_source: Option<Option<String>>,
    pub target_kind: Option<String>,
    pub markdown_path: Option<Option<String>>,
    pub script_path: Option<Option<String>>,
    pub destination_thread: Option<Option<String>>,
    pub connection_account: Option<Option<String>>,
    pub checklist: Option<Vec<ChecklistStep>>,
    pub inputs: Option<Value>,
    pub now_epoch: i64,
}

#[derive(Debug, Clone)]
pub enum SlotAdmission {
    Started(RoutineRun),
    Duplicate(RoutineRun),
    SkippedStale(RoutineRun),
    SkippedOverlap(RoutineRun),
    RepointRequired,
}

#[derive(Debug, Clone)]
pub enum RunOutcome {
    Completed {
        message_id: Option<String>,
        script_output: Option<Value>,
        checklist_state: Vec<ChecklistStep>,
    },
    SkippedEmpty {
        script_output: Option<Value>,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone)]
pub struct ConnectionPolicy {
    pub connection_id: String,
    pub provider: String,
    pub visibility: String,
    pub owner_user_id: Option<String>,
    pub is_default: bool,
    pub write_mode: String,
    pub allow_external_triggers: bool,
    pub allowed_agents: Vec<String>,
    pub policy_revision: i64,
    pub updated: f64,
}

impl ConnectionPolicy {
    pub fn default_for(connection_id: &str, provider: &str) -> Self {
        Self {
            connection_id: connection_id.to_string(),
            provider: provider.to_string(),
            visibility: "shared".to_string(),
            owner_user_id: None,
            is_default: false,
            write_mode: "ask_approval".to_string(),
            allow_external_triggers: false,
            allowed_agents: Vec::new(),
            policy_revision: 0,
            updated: 0.0,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "connection_id": self.connection_id,
            "provider": self.provider,
            "visibility": self.visibility,
            "owner_user_id": self.owner_user_id,
            "is_default": self.is_default,
            "write_mode": self.write_mode,
            "allow_external_triggers": self.allow_external_triggers,
            "allowed_agents": self.allowed_agents,
            "policy_revision": self.policy_revision,
            "updated": self.updated,
            "updated_at": self.updated,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionPolicyInput {
    pub connection_id: String,
    pub provider: String,
    pub visibility: String,
    pub owner_user_id: Option<String>,
    pub is_default: bool,
    pub write_mode: String,
    pub allow_external_triggers: bool,
    pub allowed_agents: Vec<String>,
    pub expected_policy_revision: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct StagedRoutineProposal {
    pub id: String,
    pub kind: String,
    pub proposed_by: String,
    pub state: String,
    pub title: String,
    pub payload: Value,
    pub created: f64,
    pub updated: f64,
}

impl StagedRoutineProposal {
    pub fn to_json(&self) -> Value {
        let decided_at = (self.state != "pending").then_some(self.updated);
        json!({
            "id": self.id,
            "kind": self.kind,
            "proposed_by": self.proposed_by,
            "state": self.state,
            "title": self.title,
            "payload": self.payload,
            "created": self.created,
            "created_at": self.created,
            "updated": self.updated,
            "decided_at": decided_at,
        })
    }
}

#[derive(Debug, Clone)]
pub struct CredentialRequestResolution {
    pub request_id: String,
    pub connection_id: String,
    pub message_id: String,
    pub conversation_id: Option<String>,
    pub resumed_at: f64,
}

#[derive(Debug, Clone)]
struct ParsedCron {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    dom: Option<Vec<u32>>,
    months: Option<Vec<u32>>,
    dow: Option<Vec<u32>>,
}

fn parse_cron_field(spec: &str, min: u32, max: u32, allow_sun_7: bool) -> Result<Option<Vec<u32>>> {
    if spec == "*" {
        return Ok(None);
    }
    let mut values = std::collections::BTreeSet::new();
    for part in spec.split(',') {
        if part.is_empty() {
            return Err(Error::rejected(format!("invalid cron field '{spec}'")));
        }
        let (range_str, step) = match part.split_once('/') {
            Some((lhs, rhs)) => {
                let s: u32 = rhs
                    .parse()
                    .map_err(|_| Error::rejected(format!("invalid cron step in '{spec}'")))?;
                if s == 0 {
                    return Err(Error::rejected("cron step must be >= 1"));
                }
                (lhs, s)
            }
            None => (part, 1),
        };
        let (start, end) = if range_str == "*" {
            (min, max)
        } else if let Some((a, b)) = range_str.split_once('-') {
            let start: u32 = a
                .parse()
                .map_err(|_| Error::rejected(format!("invalid cron number in '{spec}'")))?;
            let end: u32 = b
                .parse()
                .map_err(|_| Error::rejected(format!("invalid cron number in '{spec}'")))?;
            (start, end)
        } else {
            let single: u32 = range_str
                .parse()
                .map_err(|_| Error::rejected(format!("invalid cron number in '{spec}'")))?;
            (single, single)
        };
        let cap = if allow_sun_7 { 7 } else { max };
        if start < min || end > cap || start > end {
            return Err(Error::rejected(format!(
                "cron value out of range ({min}..={max}) in '{spec}'"
            )));
        }
        let mut cur = start;
        while cur <= end {
            let normalized = if allow_sun_7 && cur == 7 { 0 } else { cur };
            values.insert(normalized);
            cur += step;
        }
    }
    if values.is_empty() {
        return Err(Error::rejected(format!("empty cron field '{spec}'")));
    }
    Ok(Some(values.into_iter().collect()))
}

fn parse_cron_checked(expr: &str) -> Result<(ParsedCron, i64)> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(Error::rejected(
            "cron expression must have 5 fields: '<min> <hour> <dom> <month> <dow>'",
        ));
    }
    let minutes_opt = parse_cron_field(fields[0], 0, 59, false)?;
    let hours_opt = parse_cron_field(fields[1], 0, 23, false)?;
    let dom = parse_cron_field(fields[2], 1, 31, false)?;
    let months = parse_cron_field(fields[3], 1, 12, false)?;
    let dow = parse_cron_field(fields[4], 0, 6, true)?;

    let minutes: Vec<u32> = minutes_opt.unwrap_or_else(|| (0..=59).collect());
    let hours: Vec<u32> = hours_opt.unwrap_or_else(|| (0..=23).collect());

    // Enforce minimum 15-minute gap across matching slots within a day/wraparound.
    if minutes.len() > 1 {
        let mut min_gap = 60u32;
        for pair in minutes.windows(2) {
            min_gap = min_gap.min(pair[1] - pair[0]);
        }
        if hours.len() > 1 {
            // Check wraparound from last minute of hour H to first minute of hour H+1
            let adjacent_hours = hours.windows(2).any(|w| w[1] - w[0] == 1)
                || (hours.first() == Some(&0) && hours.last() == Some(&23));
            if adjacent_hours {
                let wrap = (60 - minutes.last().copied().unwrap_or(0))
                    + minutes.first().copied().unwrap_or(0);
                min_gap = min_gap.min(wrap);
            }
        }
        if i64::from(min_gap) < MIN_INTERVAL_MINS {
            return Err(Error::rejected(format!(
                "recurring schedule interval must be at least {MIN_INTERVAL_MINS} minutes"
            )));
        }
        return Ok((
            ParsedCron {
                minutes,
                hours,
                dom,
                months,
                dow,
            },
            i64::from(min_gap),
        ));
    }

    let derived_mins = if hours.len() == 24 {
        60
    } else if hours.len() > 1 {
        let mut hour_gap = 24u32;
        for pair in hours.windows(2) {
            hour_gap = hour_gap.min(pair[1] - pair[0]);
        }
        i64::from(hour_gap) * 60
    } else {
        1440
    };

    Ok((
        ParsedCron {
            minutes,
            hours,
            dom,
            months,
            dow,
        },
        derived_mins,
    ))
}

pub fn parse_iana_timezone(tz: &str) -> Result<Tz> {
    let trimmed = tz.trim();
    if trimmed.is_empty() {
        return Err(Error::rejected(
            "recurring schedule requires an explicit IANA timezone (for example 'UTC' or 'Asia/Hong_Kong')",
        ));
    }
    trimmed.parse::<Tz>().map_err(|_| {
        Error::rejected(format!(
            "invalid IANA timezone '{trimmed}'; expected a named zone like 'UTC' or 'America/New_York'"
        ))
    })
}

pub fn next_due_instant(
    cron: Option<&str>,
    interval_mins: Option<i64>,
    timezone: &str,
    after_epoch: i64,
) -> Result<i64> {
    let tz = parse_iana_timezone(timezone)?;
    if let Some(expr) = cron {
        let (parsed, _) = parse_cron_checked(expr)?;
        // Search minute-by-minute from the next whole minute up to 370 days ahead.
        let start_min = after_epoch.div_euclid(60) + 1;
        for offset in 0..(370 * 1440i64) {
            let candidate = (start_min + offset) * 60;
            let Some(dt_utc) = chrono::DateTime::from_timestamp(candidate, 0) else {
                break;
            };
            let local = dt_utc.with_timezone(&tz);
            if let Some(ref months) = parsed.months {
                if !months.contains(&local.month()) {
                    continue;
                }
            }
            match (&parsed.dom, &parsed.dow) {
                (Some(doms), Some(dows)) => {
                    let w = local.weekday().num_days_from_sunday();
                    if !doms.contains(&local.day()) && !dows.contains(&w) {
                        continue;
                    }
                }
                (Some(doms), None) => {
                    if !doms.contains(&local.day()) {
                        continue;
                    }
                }
                (None, Some(dows)) => {
                    let w = local.weekday().num_days_from_sunday();
                    if !dows.contains(&w) {
                        continue;
                    }
                }
                (None, None) => {}
            }
            if !parsed.hours.contains(&local.hour()) {
                continue;
            }
            if !parsed.minutes.contains(&local.minute()) {
                continue;
            }
            return Ok(candidate);
        }
        return Err(Error::rejected(
            "cron expression has no matching instant within the next year",
        ));
    }

    let mins = interval_mins.ok_or_else(|| {
        Error::rejected("schedule requires either 'cron' or 'interval_mins' (>= 15)")
    })?;
    if mins < MIN_INTERVAL_MINS {
        return Err(Error::rejected(format!(
            "recurring schedule interval must be at least {MIN_INTERVAL_MINS} minutes"
        )));
    }
    let step = mins * 60;
    let aligned = ((after_epoch / step) + 1) * step;
    // Touch `tz` to validate local conversion works cleanly.
    let _ = tz.timestamp_opt(aligned, 0);
    Ok(aligned)
}

fn validate_schedule_fields(
    trigger_kind: &str,
    cron: Option<&str>,
    interval_mins: Option<i64>,
    timezone: Option<&str>,
    event_source: Option<&str>,
    now_epoch: i64,
) -> Result<(Option<i64>, Option<i64>)> {
    match trigger_kind {
        "schedule" => {
            let tz = timezone.ok_or_else(|| {
                Error::rejected("recurring schedule requires an explicit IANA 'timezone'")
            })?;
            parse_iana_timezone(tz)?;
            let effective_interval = match (cron, interval_mins) {
                (Some(expr), explicit) => {
                    let (_, derived) = parse_cron_checked(expr)?;
                    if let Some(mins) = explicit {
                        if mins < MIN_INTERVAL_MINS {
                            return Err(Error::rejected(format!(
                                "recurring schedule interval must be at least {MIN_INTERVAL_MINS} minutes"
                            )));
                        }
                        Some(mins)
                    } else {
                        Some(derived)
                    }
                }
                (None, Some(mins)) => {
                    if mins < MIN_INTERVAL_MINS {
                        return Err(Error::rejected(format!(
                            "recurring schedule interval must be at least {MIN_INTERVAL_MINS} minutes"
                        )));
                    }
                    Some(mins)
                }
                (None, None) => {
                    return Err(Error::rejected(
                        "recurring schedule requires 'cron' or 'interval_mins' (>= 15)",
                    ))
                }
            };
            let next = next_due_instant(cron, effective_interval, tz, now_epoch)?;
            Ok((effective_interval, Some(next)))
        }
        "event" => {
            let src = event_source
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::rejected("event routine requires a non-empty 'event_source'")
                })?;
            if src.len() > 128 {
                return Err(Error::rejected("event_source is too long"));
            }
            Ok((None, None))
        }
        _ => Err(Error::rejected(
            "routine 'trigger_kind' must be 'schedule' or 'event'",
        )),
    }
}

fn validate_target_fields(
    target_kind: &str,
    markdown_path: Option<&str>,
    script_path: Option<&str>,
) -> Result<()> {
    match target_kind {
        "markdown" => {
            if markdown_path
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
            {
                return Err(Error::rejected(
                    "markdown routine requires a versioned 'markdown_path' ('wiki:...' or 'workflows/...')",
                ));
            }
            if script_path.is_some() {
                return Err(Error::rejected(
                    "markdown routine must not set 'script_path' (use 'script_then_markdown')",
                ));
            }
        }
        "script" => {
            if script_path
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
            {
                return Err(Error::rejected("script routine requires 'script_path'"));
            }
            if markdown_path.is_some() {
                return Err(Error::rejected(
                    "script routine must not set 'markdown_path' (use 'script_then_markdown')",
                ));
            }
        }
        "script_then_markdown" => {
            if script_path
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
                || markdown_path
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .is_none()
            {
                return Err(Error::rejected(
                    "script_then_markdown routine requires both 'script_path' and 'markdown_path'",
                ));
            }
        }
        _ => {
            return Err(Error::rejected(
                "routine 'target_kind' must be 'markdown', 'script', or 'script_then_markdown'",
            ))
        }
    }
    Ok(())
}

fn row_routine(row: &rusqlite::Row<'_>) -> rusqlite::Result<Routine> {
    let checklist_raw: String = row.get("checklist_json")?;
    let inputs_raw: String = row.get("inputs_json")?;
    let checklist: Vec<ChecklistStep> = serde_json::from_str(&checklist_raw).unwrap_or_default();
    let inputs: Value = serde_json::from_str(&inputs_raw).unwrap_or_else(|_| json!({}));
    Ok(Routine {
        id: row.get("id")?,
        name: row.get("name")?,
        agent_alias: row.get("agent_alias")?,
        trigger_kind: row.get("trigger_kind")?,
        cron: row.get("cron")?,
        interval_mins: row.get("interval_mins")?,
        timezone: row.get("timezone")?,
        event_source: row.get("event_source")?,
        target_kind: row.get("target_kind")?,
        markdown_path: row.get("markdown_path")?,
        approved_rev: row.get("approved_rev")?,
        script_path: row.get("script_path")?,
        approved_sha256: row.get("approved_sha256")?,
        destination_thread: row.get("destination_thread")?,
        connection_account: row.get("connection_account")?,
        checklist,
        inputs,
        status: row.get("status")?,
        pause_reason: row.get("pause_reason")?,
        consecutive_failures: row.get("consecutive_failures")?,
        next_due_at: row.get("next_due_at")?,
        last_run_at: row.get("last_run_at")?,
        last_run_state: row.get("last_run_state")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

fn row_routine_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoutineRun> {
    let checklist_raw: String = row.get("checklist_state_json")?;
    let output_raw: Option<String> = row.get("script_output_json")?;
    Ok(RoutineRun {
        id: row.get("id")?,
        routine_id: row.get("routine_id")?,
        due_instant: row.get("due_instant")?,
        trigger_source: row.get("trigger_source")?,
        state: row.get("state")?,
        approved_rev: row.get("approved_rev")?,
        approved_sha256: row.get("approved_sha256")?,
        checklist_state: serde_json::from_str(&checklist_raw).unwrap_or_default(),
        script_output: output_raw.and_then(|s| serde_json::from_str(&s).ok()),
        message_id: row.get("message_id")?,
        error: row.get("error")?,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
    })
}

fn row_connection_policy(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConnectionPolicy> {
    let agents_raw: String = row.get("allowed_agents_json")?;
    Ok(ConnectionPolicy {
        connection_id: row.get("connection_id")?,
        provider: row.get("provider")?,
        visibility: row.get("visibility")?,
        owner_user_id: row.get("owner_user_id")?,
        is_default: row.get::<_, i64>("is_default")? != 0,
        write_mode: row.get("write_mode")?,
        allow_external_triggers: row.get::<_, i64>("allow_external_triggers")? != 0,
        allowed_agents: serde_json::from_str(&agents_raw).unwrap_or_default(),
        policy_revision: row.get("policy_revision")?,
        updated: row.get("updated")?,
    })
}

fn thread_live_in(conn: &impl StoreConn, thread_id: Option<&str>) -> Result<bool> {
    let Some(tid) = thread_id else {
        return Ok(true);
    };
    let archived: Option<i64> = conn.query_opt(
        "SELECT archived FROM threads WHERE id = ?1",
        params![tid],
        |r| r.get(0),
    )?;
    Ok(matches!(archived, Some(0)))
}

fn get_routine_in(conn: &impl StoreConn, id: &str) -> Result<Routine> {
    conn.query_opt(
        "SELECT * FROM routines WHERE id = ?1",
        params![id],
        row_routine,
    )?
    .ok_or_else(|| Error::rejected(format!("routine '{id}' not found")))
}

impl Store {
    pub fn destination_thread_live(&self, thread_id: Option<&str>) -> Result<bool> {
        let conn = self.conn();
        thread_live_in(&*conn, thread_id)
    }

    pub fn routine_create(&self, input: RoutineCreate) -> Result<Routine> {
        let name = input.name.trim().to_string();
        if name.is_empty() || name.len() > 160 {
            return Err(Error::rejected("routine name must be 1..=160 characters"));
        }
        crate::proto::identifier(&input.agent_alias, "Agent alias")?;
        validate_target_fields(
            &input.target_kind,
            input.markdown_path.as_deref(),
            input.script_path.as_deref(),
        )?;
        let (effective_interval, next_due) = validate_schedule_fields(
            &input.trigger_kind,
            input.cron.as_deref(),
            input.interval_mins,
            input.timezone.as_deref(),
            input.event_source.as_deref(),
            input.now_epoch,
        )?;
        let id = format!("rtn_{}", Uuid::new_v4().simple());
        let checklist_json = serde_json::to_string(&input.checklist)?;
        let inputs_json = if input.inputs.is_object() {
            serde_json::to_string(&input.inputs)?
        } else {
            "{}".to_string()
        };

        self.write_tx(move |tx| {
            let thread_ok = thread_live_in(&*tx, input.destination_thread.as_deref())?;
            let initial_status = if !thread_ok {
                "repoint_required"
            } else {
                "waiting_approval"
            };
            let ts = now();
            tx.execute(
                "INSERT INTO routines (
                    id, name, agent_alias, trigger_kind, cron, interval_mins, timezone,
                    event_source, target_kind, markdown_path, approved_rev, script_path,
                    approved_sha256, destination_thread, connection_account,
                    checklist_json, inputs_json, status, pause_reason,
                    consecutive_failures, next_due_at, last_run_at, last_run_state,
                    created, updated
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                    ?8, ?9, ?10, NULL, ?11,
                    NULL, ?12, ?13,
                    ?14, ?15, ?16, NULL,
                    0, ?17, NULL, NULL,
                    ?18, ?18
                )",
                params![
                    id,
                    name,
                    input.agent_alias,
                    input.trigger_kind,
                    input.cron,
                    effective_interval,
                    input.timezone,
                    input.event_source,
                    input.target_kind,
                    input.markdown_path,
                    input.script_path,
                    input.destination_thread,
                    input.connection_account,
                    checklist_json,
                    inputs_json,
                    initial_status,
                    next_due,
                    ts,
                ],
            )?;
            get_routine_in(&*tx, &id)
        })
    }

    pub fn routine_get(&self, id: &str) -> Result<Routine> {
        let conn = self.conn();
        get_routine_in(&*conn, id)
    }

    pub fn routine_list(&self, agent_alias: Option<&str>) -> Result<Vec<Routine>> {
        let conn = self.conn();
        if let Some(agent) = agent_alias {
            Ok(conn.query_vec(
                "SELECT * FROM routines WHERE agent_alias = ?1 ORDER BY created ASC, id ASC",
                params![agent],
                row_routine,
            )?)
        } else {
            Ok(conn.query_vec(
                "SELECT * FROM routines ORDER BY created ASC, id ASC",
                [],
                row_routine,
            )?)
        }
    }

    pub fn routine_runs(&self, routine_id: &str, limit: usize) -> Result<Vec<RoutineRun>> {
        let conn = self.conn();
        let cap = i64::try_from(limit.clamp(1, 100)).unwrap_or(25);
        Ok(conn.query_vec(
            "SELECT * FROM routine_runs WHERE routine_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
            params![routine_id, cap],
            row_routine_run,
        )?)
    }

    /// CAD-2379: every routine's runs whose slot falls in `[from_epoch,
    /// to_epoch)`, oldest first. The calendar read bounds it by `limit`.
    pub fn routine_runs_between(
        &self,
        from_epoch: i64,
        to_epoch: i64,
        limit: usize,
    ) -> Result<Vec<RoutineRun>> {
        let conn = self.conn();
        let cap = i64::try_from(limit).unwrap_or(i64::MAX);
        Ok(conn.query_vec(
            "SELECT * FROM routine_runs WHERE due_instant >= ?1 AND due_instant < ?2 ORDER BY due_instant ASC, id ASC LIMIT ?3",
            params![from_epoch, to_epoch, cap],
            row_routine_run,
        )?)
    }

    pub fn routine_update(&self, id: &str, patch: RoutineUpdate) -> Result<Routine> {
        let id = id.to_string();
        self.write_tx(move |tx| {
            let mut cur = get_routine_in(&*tx, &id)?;
            let mut target_changed = false;
            if let Some(name) = patch.name {
                let trimmed = name.trim().to_string();
                if trimmed.is_empty() || trimmed.len() > 160 {
                    return Err(Error::rejected("routine name must be 1..=160 characters"));
                }
                cur.name = trimmed;
            }
            if let Some(tk) = patch.trigger_kind {
                cur.trigger_kind = tk;
            }
            if let Some(cron) = patch.cron {
                cur.cron = cron;
            }
            if let Some(mins) = patch.interval_mins {
                cur.interval_mins = mins;
            }
            if let Some(tz) = patch.timezone {
                cur.timezone = Some(tz);
            }
            if let Some(ev) = patch.event_source {
                cur.event_source = ev;
            }
            if let Some(tk) = patch.target_kind {
                if tk != cur.target_kind {
                    target_changed = true;
                    cur.target_kind = tk;
                }
            }
            if let Some(md) = patch.markdown_path {
                if md != cur.markdown_path {
                    target_changed = true;
                    cur.markdown_path = md;
                    cur.approved_rev = None;
                }
            }
            if let Some(sp) = patch.script_path {
                if sp != cur.script_path {
                    target_changed = true;
                    cur.script_path = sp;
                    cur.approved_sha256 = None;
                }
            }
            if let Some(dt) = patch.destination_thread {
                cur.destination_thread = dt;
            }
            if let Some(ca) = patch.connection_account {
                cur.connection_account = ca;
            }
            if let Some(cl) = patch.checklist {
                cur.checklist = cl;
            }
            if let Some(inp) = patch.inputs {
                cur.inputs = inp;
            }

            validate_target_fields(
                &cur.target_kind,
                cur.markdown_path.as_deref(),
                cur.script_path.as_deref(),
            )?;
            let (effective_interval, next_due) = validate_schedule_fields(
                &cur.trigger_kind,
                cur.cron.as_deref(),
                cur.interval_mins,
                cur.timezone.as_deref(),
                cur.event_source.as_deref(),
                patch.now_epoch,
            )?;
            cur.interval_mins = effective_interval;
            cur.next_due_at = next_due;

            let thread_ok = thread_live_in(&*tx, cur.destination_thread.as_deref())?;
            let approval_ready = (!cur.requires_markdown() || cur.approved_rev.is_some())
                && (!cur.requires_script() || cur.approved_sha256.is_some());

            if !thread_ok {
                cur.status = "repoint_required".to_string();
                cur.pause_reason = Some("destination_thread_missing".to_string());
            } else if target_changed || !approval_ready {
                if cur.status != "paused" {
                    cur.status = "waiting_approval".to_string();
                }
            } else if cur.status == "repoint_required" {
                cur.status = "on".to_string();
                cur.pause_reason = None;
            }

            let ts = now();
            let checklist_json = serde_json::to_string(&cur.checklist)?;
            let inputs_json = serde_json::to_string(&cur.inputs)?;
            tx.execute(
                "UPDATE routines SET
                    name = ?2, trigger_kind = ?3, cron = ?4, interval_mins = ?5, timezone = ?6,
                    event_source = ?7, target_kind = ?8, markdown_path = ?9,
                    approved_rev = ?10, script_path = ?11, approved_sha256 = ?12,
                    destination_thread = ?13, connection_account = ?14,
                    checklist_json = ?15, inputs_json = ?16, status = ?17,
                    pause_reason = ?18, next_due_at = ?19, updated = ?20
                 WHERE id = ?1",
                params![
                    cur.id,
                    cur.name,
                    cur.trigger_kind,
                    cur.cron,
                    cur.interval_mins,
                    cur.timezone,
                    cur.event_source,
                    cur.target_kind,
                    cur.markdown_path,
                    cur.approved_rev,
                    cur.script_path,
                    cur.approved_sha256,
                    cur.destination_thread,
                    cur.connection_account,
                    checklist_json,
                    inputs_json,
                    cur.status,
                    cur.pause_reason,
                    cur.next_due_at,
                    ts,
                ],
            )?;
            get_routine_in(&*tx, &cur.id)
        })
    }

    pub fn routine_approve(
        &self,
        id: &str,
        approved_rev: Option<String>,
        approved_sha256: Option<String>,
        now_epoch: i64,
    ) -> Result<Routine> {
        let id = id.to_string();
        self.write_tx(move |tx| {
            let cur = get_routine_in(&*tx, &id)?;
            if cur.requires_markdown() && approved_rev.is_none() {
                return Err(Error::rejected(
                    "approving a markdown routine requires 'expect_rev'",
                ));
            }
            if cur.requires_script() && approved_sha256.is_none() {
                return Err(Error::rejected(
                    "approving a script routine requires 'digest' (sha256)",
                ));
            }
            let thread_ok = thread_live_in(&*tx, cur.destination_thread.as_deref())?;
            let next_status = if !thread_ok { "repoint_required" } else { "on" };
            let next_due = if cur.trigger_kind == "schedule" {
                Some(next_due_instant(
                    cur.cron.as_deref(),
                    cur.interval_mins,
                    cur.timezone.as_deref().unwrap_or("UTC"),
                    now_epoch,
                )?)
            } else {
                None
            };
            let ts = now();
            tx.execute(
                "UPDATE routines SET
                    approved_rev = ?2,
                    approved_sha256 = ?3,
                    status = ?4,
                    pause_reason = ?5,
                    consecutive_failures = 0,
                    next_due_at = ?6,
                    updated = ?7
                 WHERE id = ?1",
                params![
                    id,
                    approved_rev,
                    approved_sha256,
                    next_status,
                    (!thread_ok).then_some("destination_thread_missing"),
                    next_due,
                    ts,
                ],
            )?;
            get_routine_in(&*tx, &id)
        })
    }

    pub fn routine_pause(&self, id: &str, reason: Option<&str>) -> Result<Routine> {
        let id = id.to_string();
        let reason = reason.unwrap_or("operator_paused").to_string();
        self.write_tx(move |tx| {
            let _ = get_routine_in(&*tx, &id)?;
            tx.execute(
                "UPDATE routines SET status = 'paused', pause_reason = ?2, updated = ?3 WHERE id = ?1",
                params![id, reason, now()],
            )?;
            get_routine_in(&*tx, &id)
        })
    }

    pub fn routine_resume(&self, id: &str, now_epoch: i64) -> Result<Routine> {
        let id = id.to_string();
        self.write_tx(move |tx| {
            let cur = get_routine_in(&*tx, &id)?;
            let thread_ok = thread_live_in(&*tx, cur.destination_thread.as_deref())?;
            let approval_ready = (!cur.requires_markdown() || cur.approved_rev.is_some())
                && (!cur.requires_script() || cur.approved_sha256.is_some());
            let next_status = if !thread_ok {
                "repoint_required"
            } else if !approval_ready {
                "waiting_approval"
            } else {
                "on"
            };
            let next_due = if cur.trigger_kind == "schedule" {
                Some(next_due_instant(
                    cur.cron.as_deref(),
                    cur.interval_mins,
                    cur.timezone.as_deref().unwrap_or("UTC"),
                    now_epoch,
                )?)
            } else {
                None
            };
            tx.execute(
                "UPDATE routines SET
                    status = ?2,
                    pause_reason = ?3,
                    consecutive_failures = 0,
                    next_due_at = ?4,
                    updated = ?5
                 WHERE id = ?1",
                params![
                    id,
                    next_status,
                    (!thread_ok).then_some("destination_thread_missing"),
                    next_due,
                    now(),
                ],
            )?;
            get_routine_in(&*tx, &id)
        })
    }

    pub fn routine_remove(&self, id: &str) -> Result<Routine> {
        let id = id.to_string();
        self.write_tx(move |tx| {
            let cur = get_routine_in(&*tx, &id)?;
            tx.execute(
                "DELETE FROM routine_runs WHERE routine_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM routines WHERE id = ?1", params![id])?;
            Ok(cur)
        })
    }

    /// Due routines at `now_epoch` (`status = 'on'` and `next_due_at <= now_epoch`).
    pub fn routines_due(&self, now_epoch: i64) -> Result<Vec<Routine>> {
        let conn = self.conn();
        Ok(conn.query_vec(
            "SELECT * FROM routines WHERE status = 'on' AND trigger_kind = 'schedule' AND next_due_at IS NOT NULL AND next_due_at <= ?1 ORDER BY next_due_at ASC, id ASC",
            params![now_epoch],
            row_routine,
        )?)
    }

    /// Pick a fresh ad-hoc `due_instant` anchored at `now_epoch` when a manual or
    /// un-keyed external trigger omits an explicit slot key, avoiding collision
    /// with a prior ad-hoc run in the same wall-clock second.
    pub fn next_adhoc_slot(&self, routine_id: &str, now_epoch: i64) -> Result<i64> {
        let conn = self.conn();
        let mut candidate = now_epoch;
        for _ in 0..1024 {
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM routine_runs WHERE routine_id = ?1 AND due_instant = ?2",
                params![routine_id, candidate],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Ok(candidate);
            }
            candidate -= 1;
        }
        Ok(candidate)
    }

    /// Deterministic `(routine_id, due_instant)` slot admission with 10-minute
    /// stale skip, single-flight overlap guard, and deleted-thread repoint fence.
    pub fn routine_admit_slot(
        &self,
        routine_id: &str,
        due_instant: i64,
        now_epoch: i64,
        trigger_source: &str,
    ) -> Result<SlotAdmission> {
        let routine_id = routine_id.to_string();
        let trigger_source = trigger_source.to_string();
        self.write_tx(move |tx| {
            if let Some(existing) = tx.query_opt(
                "SELECT * FROM routine_runs WHERE routine_id = ?1 AND due_instant = ?2",
                params![routine_id, due_instant],
                row_routine_run,
            )? {
                return Ok(SlotAdmission::Duplicate(existing));
            }

            let cur = get_routine_in(&*tx, &routine_id)?;
            if !thread_live_in(&*tx, cur.destination_thread.as_deref())? {
                tx.execute(
                    "UPDATE routines SET status = 'repoint_required', pause_reason = 'destination_thread_missing', updated = ?2 WHERE id = ?1",
                    params![routine_id, now()],
                )?;
                return Ok(SlotAdmission::RepointRequired);
            }

            if cur.status != "on" {
                return Err(Error::rejected(format!(
                    "routine '{}' cannot run while status is '{}'",
                    cur.id, cur.status
                )));
            }
            if (cur.requires_markdown() && cur.approved_rev.is_none())
                || (cur.requires_script() && cur.approved_sha256.is_none())
            {
                return Err(Error::rejected(
                    "unapproved_target: routine requires owner approval before running",
                ));
            }

            let advance_next = if cur.trigger_kind == "schedule" && trigger_source == "schedule" {
                let anchor = now_epoch.max(due_instant);
                Some(next_due_instant(
                    cur.cron.as_deref(),
                    cur.interval_mins,
                    cur.timezone.as_deref().unwrap_or("UTC"),
                    anchor,
                )?)
            } else {
                cur.next_due_at
            };

            let ts = now();
            let run_id = format!("rrun_{}", Uuid::new_v4().simple());
            let checklist_json = serde_json::to_string(&cur.checklist)?;

            // 10-minute stale-slot skip for scheduled runs (never replay a burst after downtime).
            if trigger_source == "schedule" && now_epoch.saturating_sub(due_instant) > STALE_SLOT_SECS {
                tx.execute(
                    "INSERT INTO routine_runs (
                        id, routine_id, due_instant, trigger_source, state,
                        approved_rev, approved_sha256, checklist_state_json,
                        script_output_json, message_id, error, started_at, finished_at
                    ) VALUES (?1, ?2, ?3, ?4, 'skipped_stale', ?5, ?6, ?7, NULL, NULL, 'slot exceeded 10m staleness window', ?8, ?8)",
                    params![
                        run_id,
                        routine_id,
                        due_instant,
                        trigger_source,
                        cur.approved_rev,
                        cur.approved_sha256,
                        checklist_json,
                        ts,
                    ],
                )?;
                tx.execute(
                    "UPDATE routines SET next_due_at = ?2, last_run_at = ?3, last_run_state = 'skipped_stale', updated = ?4 WHERE id = ?1",
                    params![routine_id, advance_next, due_instant, ts],
                )?;
                let run = tx
                    .query_opt("SELECT * FROM routine_runs WHERE id = ?1", params![run_id], row_routine_run)?
                    .ok_or_else(|| Error::internal("missing inserted stale run"))?;
                return Ok(SlotAdmission::SkippedStale(run));
            }

            // Single-flight overlap guard.
            let in_flight: i64 = tx.query_row(
                "SELECT COUNT(*) FROM routine_runs WHERE routine_id = ?1 AND state IN ('running', 'waiting_approval')",
                params![routine_id],
                |r| r.get(0),
            )?;
            if in_flight > 0 {
                if trigger_source == "schedule" {
                    tx.execute(
                        "INSERT INTO routine_runs (
                            id, routine_id, due_instant, trigger_source, state,
                            approved_rev, approved_sha256, checklist_state_json,
                            script_output_json, message_id, error, started_at, finished_at
                        ) VALUES (?1, ?2, ?3, ?4, 'skipped_overlap', ?5, ?6, ?7, NULL, NULL, 'previous run still in flight', ?8, ?8)",
                        params![
                            run_id,
                            routine_id,
                            due_instant,
                            trigger_source,
                            cur.approved_rev,
                            cur.approved_sha256,
                            checklist_json,
                            ts,
                        ],
                    )?;
                    tx.execute(
                        "UPDATE routines SET next_due_at = ?2, updated = ?3 WHERE id = ?1",
                        params![routine_id, advance_next, ts],
                    )?;
                    let run = tx
                        .query_opt("SELECT * FROM routine_runs WHERE id = ?1", params![run_id], row_routine_run)?
                        .ok_or_else(|| Error::internal("missing inserted overlap run"))?;
                    return Ok(SlotAdmission::SkippedOverlap(run));
                }
                return Err(Error::rejected(
                    "routine_overlap: another run for this routine is still in progress or awaiting approval",
                ));
            }

            tx.execute(
                "INSERT INTO routine_runs (
                    id, routine_id, due_instant, trigger_source, state,
                    approved_rev, approved_sha256, checklist_state_json,
                    script_output_json, message_id, error, started_at, finished_at
                ) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, NULL, NULL, NULL, ?8, NULL)",
                params![
                    run_id,
                    routine_id,
                    due_instant,
                    trigger_source,
                    cur.approved_rev,
                    cur.approved_sha256,
                    checklist_json,
                    ts,
                ],
            )?;
            tx.execute(
                "UPDATE routines SET next_due_at = ?2, last_run_at = ?3, last_run_state = 'running', updated = ?4 WHERE id = ?1",
                params![routine_id, advance_next, due_instant, ts],
            )?;
            let run = tx
                .query_opt("SELECT * FROM routine_runs WHERE id = ?1", params![run_id], row_routine_run)?
                .ok_or_else(|| Error::internal("missing inserted routine run"))?;
            Ok(SlotAdmission::Started(run))
        })
    }

    pub fn routine_finish_run(&self, run_id: &str, outcome: RunOutcome) -> Result<RoutineRun> {
        let run_id = run_id.to_string();
        self.write_tx(move |tx| {
            let run = tx
                .query_opt(
                    "SELECT * FROM routine_runs WHERE id = ?1",
                    params![run_id],
                    row_routine_run,
                )?
                .ok_or_else(|| Error::rejected(format!("routine run '{run_id}' not found")))?;
            let cur = get_routine_in(&*tx, &run.routine_id)?;
            let ts = now();

            match outcome {
                RunOutcome::Completed {
                    message_id,
                    script_output,
                    checklist_state,
                } => {
                    let holds_approval = checklist_state.iter().any(|s| {
                        s.requires_approval && !matches!(s.state.as_str(), "completed" | "approved")
                    });
                    let final_state = if holds_approval {
                        "waiting_approval"
                    } else {
                        "completed"
                    };
                    let finished_at = (!holds_approval).then_some(ts);
                    let checklist_json = serde_json::to_string(&checklist_state)?;
                    let output_json = script_output
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?;
                    tx.execute(
                        "UPDATE routine_runs SET
                            state = ?2,
                            checklist_state_json = ?3,
                            script_output_json = ?4,
                            message_id = ?5,
                            finished_at = ?6
                         WHERE id = ?1",
                        params![
                            run_id,
                            final_state,
                            checklist_json,
                            output_json,
                            message_id,
                            finished_at,
                        ],
                    )?;
                    tx.execute(
                        "UPDATE routines SET consecutive_failures = 0, last_run_state = ?2, updated = ?3 WHERE id = ?1",
                        params![cur.id, final_state, ts],
                    )?;
                }
                RunOutcome::SkippedEmpty { script_output } => {
                    let output_json = script_output
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?;
                    tx.execute(
                        "UPDATE routine_runs SET
                            state = 'skipped_empty',
                            script_output_json = ?2,
                            finished_at = ?3
                         WHERE id = ?1",
                        params![run_id, output_json, ts],
                    )?;
                    tx.execute(
                        "UPDATE routines SET consecutive_failures = 0, last_run_state = 'skipped_empty', updated = ?2 WHERE id = ?1",
                        params![cur.id, ts],
                    )?;
                }
                RunOutcome::Failed { error } => {
                    let failures = cur.consecutive_failures.saturating_add(1);
                    let auto_pause = failures >= AUTO_PAUSE_FAILURES;
                    let next_status = if auto_pause {
                        "paused"
                    } else {
                        cur.status.as_str()
                    };
                    let pause_reason = if auto_pause {
                        Some("auto_paused_failures".to_string())
                    } else {
                        cur.pause_reason.clone()
                    };
                    tx.execute(
                        "UPDATE routine_runs SET state = 'failed', error = ?2, finished_at = ?3 WHERE id = ?1",
                        params![run_id, error, ts],
                    )?;
                    tx.execute(
                        "UPDATE routines SET
                            consecutive_failures = ?2,
                            status = ?3,
                            pause_reason = ?4,
                            last_run_state = 'failed',
                            updated = ?5
                         WHERE id = ?1",
                        params![cur.id, failures, next_status, pause_reason, ts],
                    )?;
                }
            }

            tx.query_opt(
                "SELECT * FROM routine_runs WHERE id = ?1",
                params![run_id],
                row_routine_run,
            )?
            .ok_or_else(|| Error::internal("missing updated routine run"))
        })
    }

    pub fn connection_policy_get(
        &self,
        connection_id: &str,
        fallback_provider: &str,
    ) -> Result<ConnectionPolicy> {
        self.connection_policy_resolve(connection_id, fallback_provider, "")
    }

    pub fn connection_policy_resolve(
        &self,
        connection_id: &str,
        fallback_provider: &str,
        account: &str,
    ) -> Result<ConnectionPolicy> {
        let conn = self.conn();
        if let Some(found) = conn.query_opt(
            "SELECT * FROM connection_policies WHERE connection_id = ?1",
            params![connection_id],
            row_connection_policy,
        )? {
            return Ok(found);
        }
        if !fallback_provider.is_empty() && !account.is_empty() {
            let acct_key = format!("acct:{fallback_provider}:{account}");
            if let Some(mut inherited) = conn.query_opt(
                "SELECT * FROM connection_policies WHERE connection_id = ?1",
                params![acct_key],
                row_connection_policy,
            )? {
                inherited.connection_id = connection_id.to_string();
                return Ok(inherited);
            }
        }
        Ok(ConnectionPolicy::default_for(
            connection_id,
            fallback_provider,
        ))
    }

    pub fn connection_policies_all(&self) -> Result<Vec<ConnectionPolicy>> {
        let conn = self.conn();
        Ok(conn.query_vec(
            "SELECT * FROM connection_policies ORDER BY connection_id ASC",
            [],
            row_connection_policy,
        )?)
    }

    pub fn connection_policy_set(&self, input: ConnectionPolicyInput) -> Result<ConnectionPolicy> {
        if !matches!(input.visibility.as_str(), "personal" | "shared") {
            return Err(Error::rejected(
                "connection policy 'visibility' must be 'personal' or 'shared'",
            ));
        }
        if !matches!(input.write_mode.as_str(), "ask_approval" | "read_only") {
            return Err(Error::rejected(
                "connection policy 'write_mode' must be 'ask_approval' or 'read_only'",
            ));
        }
        for alias in &input.allowed_agents {
            crate::proto::identifier(alias, "Allowed agent alias")?;
        }

        self.write_tx(move |tx| {
            let existing = tx.query_opt(
                "SELECT * FROM connection_policies WHERE connection_id = ?1",
                params![input.connection_id],
                row_connection_policy,
            )?;
            if let Some(expected_rev) = input.expected_policy_revision {
                let current_rev = existing.as_ref().map_or(0, |p| p.policy_revision);
                if expected_rev != current_rev {
                    return Err(Error::invalid(
                        "stale_revision",
                        "connection policy revision changed before save",
                    ));
                }
            }
            let provider = if !input.provider.is_empty() {
                input.provider.clone()
            } else {
                existing
                    .as_ref()
                    .map(|e| e.provider.clone())
                    .unwrap_or_default()
            };
            let is_default = input.visibility == "personal" && input.is_default;
            let allow_external =
                if input.visibility == "shared" { input.allow_external_triggers } else { false };
            if is_default && !provider.is_empty() {
                tx.execute(
                    "UPDATE connection_policies SET is_default = 0 WHERE provider = ?1 AND visibility = 'personal' AND connection_id != ?2",
                    params![provider, input.connection_id],
                )?;
            }
            let next_rev = existing.as_ref().map_or(1, |e| e.policy_revision + 1);
            let agents_json = serde_json::to_string(&input.allowed_agents)?;
            let ts = now();
            tx.execute(
                "INSERT INTO connection_policies (
                    connection_id, provider, visibility, owner_user_id, is_default,
                    write_mode, allow_external_triggers, allowed_agents_json,
                    policy_revision, updated
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(connection_id) DO UPDATE SET
                    provider = excluded.provider,
                    visibility = excluded.visibility,
                    owner_user_id = excluded.owner_user_id,
                    is_default = excluded.is_default,
                    write_mode = excluded.write_mode,
                    allow_external_triggers = excluded.allow_external_triggers,
                    allowed_agents_json = excluded.allowed_agents_json,
                    policy_revision = excluded.policy_revision,
                    updated = excluded.updated",
                params![
                    input.connection_id,
                    provider,
                    input.visibility,
                    input.owner_user_id,
                    i64::from(is_default),
                    input.write_mode,
                    i64::from(allow_external),
                    agents_json,
                    next_rev,
                    ts,
                ],
            )?;
            tx.query_opt(
                "SELECT * FROM connection_policies WHERE connection_id = ?1",
                params![input.connection_id],
                row_connection_policy,
            )?
            .ok_or_else(|| Error::internal("missing saved connection policy"))
        })
    }

    pub fn staged_proposal_create(
        &self,
        kind: &str,
        proposed_by: &str,
        title: &str,
        payload: &Value,
    ) -> Result<StagedRoutineProposal> {
        if !matches!(kind, "wiki_propose" | "routine_propose") {
            return Err(Error::rejected(
                "proposal kind must be 'wiki_propose' or 'routine_propose'",
            ));
        }
        let title = title.trim().to_string();
        if title.is_empty() || title.len() > 200 {
            return Err(Error::rejected("proposal title must be 1..=200 characters"));
        }
        let id = format!("rprop_{}", Uuid::new_v4().simple());
        let kind = kind.to_string();
        let proposed_by = proposed_by.to_string();
        let payload_json = serde_json::to_string(payload)?;
        let payload_clone = payload.clone();
        self.write_tx(move |tx| {
            let ts = now();
            tx.execute(
                "INSERT INTO staged_routine_proposals (
                    id, kind, proposed_by, state, title, payload_json, created, updated
                ) VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6, ?6)",
                params![id, kind, proposed_by, title, payload_json, ts],
            )?;
            Ok(StagedRoutineProposal {
                id,
                kind,
                proposed_by,
                state: "pending".to_string(),
                title,
                payload: payload_clone,
                created: ts,
                updated: ts,
            })
        })
    }

    pub fn staged_proposals_pending(&self) -> Result<Vec<StagedRoutineProposal>> {
        let conn = self.conn();
        Ok(conn.query_vec(
            "SELECT * FROM staged_routine_proposals WHERE state = 'pending' ORDER BY created DESC, id DESC",
            [],
            |row| {
                let raw: String = row.get("payload_json")?;
                Ok(StagedRoutineProposal {
                    id: row.get("id")?,
                    kind: row.get("kind")?,
                    proposed_by: row.get("proposed_by")?,
                    state: row.get("state")?,
                    title: row.get("title")?,
                    payload: serde_json::from_str(&raw).unwrap_or_else(|_| json!({})),
                    created: row.get("created")?,
                    updated: row.get("updated")?,
                })
            },
        )?)
    }

    pub fn staged_proposal_get(&self, proposal_id: &str) -> Result<StagedRoutineProposal> {
        let conn = self.conn();
        conn.query_opt(
            "SELECT * FROM staged_routine_proposals WHERE id = ?1",
            params![proposal_id],
            |row| {
                let raw: String = row.get("payload_json")?;
                Ok(StagedRoutineProposal {
                    id: row.get("id")?,
                    kind: row.get("kind")?,
                    proposed_by: row.get("proposed_by")?,
                    state: row.get("state")?,
                    title: row.get("title")?,
                    payload: serde_json::from_str(&raw).unwrap_or_else(|_| json!({})),
                    created: row.get("created")?,
                    updated: row.get("updated")?,
                })
            },
        )?
        .ok_or_else(|| Error::rejected(format!("staged proposal '{proposal_id}' not found")))
    }

    pub fn staged_proposal_decide(
        &self,
        proposal_id: &str,
        decision: &str,
    ) -> Result<StagedRoutineProposal> {
        let next_state = match decision {
            "approve" | "approved" => "approved",
            "dismiss" | "dismissed" | "reject" | "rejected" => "dismissed",
            _ => {
                return Err(Error::rejected(
                    "proposal decision must be 'approve' or 'dismiss'",
                ))
            }
        };
        let id = proposal_id.to_string();
        let next_state = next_state.to_string();
        self.write_tx(move |tx| {
            let existing = tx
                .query_opt(
                    "SELECT * FROM staged_routine_proposals WHERE id = ?1",
                    params![id],
                    |row| {
                        let raw: String = row.get("payload_json")?;
                        Ok(StagedRoutineProposal {
                            id: row.get("id")?,
                            kind: row.get("kind")?,
                            proposed_by: row.get("proposed_by")?,
                            state: row.get("state")?,
                            title: row.get("title")?,
                            payload: serde_json::from_str(&raw).unwrap_or_else(|_| json!({})),
                            created: row.get("created")?,
                            updated: row.get("updated")?,
                        })
                    },
                )?
                .ok_or_else(|| Error::rejected(format!("staged proposal '{id}' not found")))?;
            if existing.state != "pending" {
                return Err(Error::rejected(format!(
                    "staged proposal '{id}' is already '{}'",
                    existing.state
                )));
            }
            let ts = now();
            tx.execute(
                "UPDATE staged_routine_proposals SET state = ?2, updated = ?3 WHERE id = ?1",
                params![id, next_state, ts],
            )?;
            Ok(StagedRoutineProposal {
                state: next_state,
                updated: ts,
                ..existing
            })
        })
    }

    pub fn routine_mark_repoint_required(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.write_tx(move |tx| {
            tx.execute(
                "UPDATE routines SET status = 'repoint_required', pause_reason = 'destination_thread_missing', updated = ?2 WHERE id = ?1 AND status = 'on'",
                params![id, now()],
            )?;
            Ok(())
        })
    }

    pub fn routine_mark_approval_drift(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.write_tx(move |tx| {
            tx.execute(
                "UPDATE routines SET status = 'waiting_approval', pause_reason = 'approval_drift', updated = ?2 WHERE id = ?1 AND status = 'on'",
                params![id, now()],
            )?;
            Ok(())
        })
    }

    pub fn routine_run_get(&self, run_id: &str) -> Result<RoutineRun> {
        let conn = self.conn();
        conn.query_opt(
            "SELECT * FROM routine_runs WHERE id = ?1",
            params![run_id],
            row_routine_run,
        )?
        .ok_or_else(|| Error::rejected(format!("routine run '{run_id}' not found")))
    }

    pub fn routine_runs_waiting_approval(&self) -> Result<Vec<(Routine, RoutineRun)>> {
        let conn = self.conn();
        let runs = conn.query_vec(
            "SELECT * FROM routine_runs WHERE state = 'waiting_approval' ORDER BY started_at DESC, id DESC",
            [],
            row_routine_run,
        )?;
        let mut out = Vec::with_capacity(runs.len());
        for run in runs {
            if let Ok(routine) = get_routine_in(&*conn, &run.routine_id) {
                out.push((routine, run));
            }
        }
        Ok(out)
    }

    pub fn routine_run_decide(
        &self,
        run_id: &str,
        decision: &str,
        follow_up_message_id: Option<&str>,
    ) -> Result<RoutineRun> {
        let approve = match decision {
            "approve" | "approved" => true,
            "dismiss" | "dismissed" | "reject" | "rejected" => false,
            _ => {
                return Err(Error::rejected(
                    "routine run decision must be 'approve' or 'dismiss'",
                ))
            }
        };
        let run_id = run_id.to_string();
        let follow_up_msg = follow_up_message_id.map(str::to_string);
        self.write_tx(move |tx| {
            let mut run = tx
                .query_opt(
                    "SELECT * FROM routine_runs WHERE id = ?1",
                    params![run_id],
                    row_routine_run,
                )?
                .ok_or_else(|| Error::rejected(format!("routine run '{run_id}' not found")))?;
            if run.state != "waiting_approval" {
                return Err(Error::rejected(format!(
                    "routine run '{run_id}' is '{}' and is not waiting for approval",
                    run.state
                )));
            }
            for step in &mut run.checklist_state {
                if step.state == "waiting_approval" || step.requires_approval {
                    step.state = if approve {
                        "approved".to_string()
                    } else {
                        "dismissed".to_string()
                    };
                }
            }
            let final_state = if approve { "completed" } else { "dismissed" };
            let checklist_json = serde_json::to_string(&run.checklist_state)?;
            let message_id = follow_up_msg.or(run.message_id);
            let ts = now();
            tx.execute(
                "UPDATE routine_runs SET state = ?2, checklist_state_json = ?3, message_id = ?4, finished_at = ?5 WHERE id = ?1",
                params![run_id, final_state, checklist_json, message_id, ts],
            )?;
            tx.execute(
                "UPDATE routines SET last_run_state = ?2, updated = ?3 WHERE id = ?1",
                params![run.routine_id, final_state, ts],
            )?;
            tx.query_opt(
                "SELECT * FROM routine_runs WHERE id = ?1",
                params![run_id],
                row_routine_run,
            )?
            .ok_or_else(|| Error::internal("missing updated routine run"))
        })
    }

    pub fn credential_request_resolution(
        &self,
        request_id: &str,
    ) -> Result<Option<CredentialRequestResolution>> {
        let conn = self.conn();
        Ok(conn.query_opt(
            "SELECT * FROM credential_request_resolutions WHERE request_id = ?1",
            params![request_id],
            |row| {
                Ok(CredentialRequestResolution {
                    request_id: row.get("request_id")?,
                    connection_id: row.get("connection_id")?,
                    message_id: row.get("message_id")?,
                    conversation_id: row.get("conversation_id")?,
                    resumed_at: row.get("resumed_at")?,
                })
            },
        )?)
    }

    pub fn record_credential_request_resolution(
        &self,
        request_id: &str,
        connection_id: &str,
        message_id: &str,
        conversation_id: Option<&str>,
    ) -> Result<CredentialRequestResolution> {
        let req_id = request_id.to_string();
        let conn_id = connection_id.to_string();
        let msg_id = message_id.to_string();
        let conv_id = conversation_id.map(str::to_string);
        self.write_tx(move |tx| {
            let existing: i64 = tx.query_row(
                "SELECT COUNT(*) FROM credential_request_resolutions WHERE request_id = ?1",
                params![req_id],
                |r| r.get(0),
            )?;
            if existing > 0 {
                return Err(Error::rejected(
                    "credential_request_already_resumed: paused reply was already continued",
                ));
            }
            let ts = now();
            tx.execute(
                "INSERT INTO credential_request_resolutions (
                    request_id, connection_id, message_id, conversation_id, resumed_at
                ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![req_id, conn_id, msg_id, conv_id, ts],
            )?;
            Ok(CredentialRequestResolution {
                request_id: req_id,
                connection_id: conn_id,
                message_id: msg_id,
                conversation_id: conv_id,
                resumed_at: ts,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("cadence.sqlite3")).unwrap();
        (dir, store)
    }

    fn base_create(now_epoch: i64) -> RoutineCreate {
        RoutineCreate {
            name: "Morning inbox triage".to_string(),
            agent_alias: "assistant".to_string(),
            trigger_kind: "schedule".to_string(),
            cron: Some("0 9 * * 1-5".to_string()),
            interval_mins: None,
            timezone: Some("Asia/Hong_Kong".to_string()),
            event_source: None,
            target_kind: "markdown".to_string(),
            markdown_path: Some("wiki:global/triage.md".to_string()),
            script_path: None,
            destination_thread: None,
            connection_account: None,
            checklist: vec![ChecklistStep {
                id: "s1".to_string(),
                label: "Summarize urgent threads".to_string(),
                requires_approval: false,
                state: "pending".to_string(),
            }],
            inputs: json!({}),
            now_epoch,
        }
    }

    #[test]
    fn rejects_sub_15_minute_schedules_and_invalid_timezones() {
        let (_dir, store) = test_store();
        let mut fast_interval = base_create(1_760_000_000);
        fast_interval.cron = None;
        fast_interval.interval_mins = Some(10);
        assert!(store.routine_create(fast_interval).is_err());

        let mut fast_cron = base_create(1_760_000_000);
        fast_cron.cron = Some("*/5 * * * *".to_string());
        assert!(store.routine_create(fast_cron).is_err());

        let mut bad_tz = base_create(1_760_000_000);
        bad_tz.timezone = Some("Mars/Olympus_Mons".to_string());
        assert!(store.routine_create(bad_tz).is_err());

        let ok = store.routine_create(base_create(1_760_000_000)).unwrap();
        assert_eq!(ok.status, "waiting_approval");
    }

    #[test]
    fn enforces_idempotency_stale_skip_overlap_and_10_failure_autopause() {
        let (_dir, store) = test_store();
        let created = store.routine_create(base_create(1_760_000_000)).unwrap();
        let approved = store
            .routine_approve(
                &created.id,
                Some("fnv1a:0123456789abcdef".to_string()),
                None,
                1_760_000_000,
            )
            .unwrap();
        assert_eq!(approved.status, "on");

        // 1. Stale slot (> 600s late) skips without running or replaying burst.
        let due_stale = 1_760_000_900;
        let stale = store
            .routine_admit_slot(&created.id, due_stale, due_stale + 601, "schedule")
            .unwrap();
        match stale {
            SlotAdmission::SkippedStale(r) => assert_eq!(r.state, "skipped_stale"),
            other => panic!("expected SkippedStale, got {other:?}"),
        }

        // 2. Idempotent deduplication for same (routine_id, due_instant).
        let dup = store
            .routine_admit_slot(&created.id, due_stale, due_stale + 602, "schedule")
            .unwrap();
        assert!(matches!(dup, SlotAdmission::Duplicate(_)));

        // 3. Single-flight overlap guard while run is active.
        let due_live = 1_760_003_600;
        let started = match store
            .routine_admit_slot(&created.id, due_live, due_live + 5, "schedule")
            .unwrap()
        {
            SlotAdmission::Started(r) => r,
            other => panic!("expected Started, got {other:?}"),
        };
        let overlap = store
            .routine_admit_slot(&created.id, due_live + 900, due_live + 905, "schedule")
            .unwrap();
        assert!(matches!(overlap, SlotAdmission::SkippedOverlap(_)));
        store
            .routine_finish_run(
                &started.id,
                RunOutcome::Completed {
                    message_id: Some("msg_1".to_string()),
                    script_output: None,
                    checklist_state: created.checklist.clone(),
                },
            )
            .unwrap();

        // 4. Ten consecutive failures auto-pause the routine.
        for idx in 0..10 {
            let slot = 1_760_010_000 + idx * 900;
            let run = match store
                .routine_admit_slot(&created.id, slot, slot + 1, "schedule")
                .unwrap()
            {
                SlotAdmission::Started(r) => r,
                other => panic!("expected Started on failure #{idx}, got {other:?}"),
            };
            store
                .routine_finish_run(
                    &run.id,
                    RunOutcome::Failed {
                        error: format!("upstream timeout #{idx}"),
                    },
                )
                .unwrap();
        }
        let paused = store.routine_get(&created.id).unwrap();
        assert_eq!(paused.status, "paused");
        assert_eq!(paused.pause_reason.as_deref(), Some("auto_paused_failures"));
        assert_eq!(paused.consecutive_failures, 10);
    }

    #[test]
    fn missing_destination_thread_transitions_to_repoint_required_and_recovers() {
        let (_dir, store) = test_store();
        let mut create = base_create(1_760_000_000);
        create.destination_thread = Some("thr_deleted".to_string());
        let created = store.routine_create(create).unwrap();
        assert_eq!(created.status, "repoint_required");

        let approved = store
            .routine_approve(
                &created.id,
                Some("fnv1a:0123456789abcdef".to_string()),
                None,
                1_760_000_000,
            )
            .unwrap();
        assert_eq!(approved.status, "repoint_required");

        // Clearing destination_thread (or pointing to a live thread) restores status = 'on'.
        let repointed = store
            .routine_update(
                &created.id,
                RoutineUpdate {
                    name: None,
                    trigger_kind: None,
                    cron: None,
                    interval_mins: None,
                    timezone: None,
                    event_source: None,
                    target_kind: None,
                    markdown_path: None,
                    script_path: None,
                    destination_thread: Some(None),
                    connection_account: None,
                    checklist: None,
                    inputs: None,
                    now_epoch: 1_760_000_100,
                },
            )
            .unwrap();
        assert_eq!(repointed.status, "on");
    }
}
