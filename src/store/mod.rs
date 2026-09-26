//! SQLite persistence: agents, durable message queue, event log.
//!
//! Single-writer discipline: one `Connection` behind a `Mutex`; every
//! mutation runs inside `BEGIN IMMEDIATE`. Provider submission and SQLite
//! are never one transaction — ambiguous in-flight attempts are preserved
//! as `unknown` for human review instead of replayed.
//!
//! Message states: queued -> submitting -> running ->
//!   completed | failed | interrupted | unknown
//! Agent states: starting -> idle <-> busy -> waiting_input ->
//!   attention | stopping -> stopped | offline

use crate::error::{Error, Result};
use rusqlite::Connection;
use serde_json::json;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod platform;
pub(crate) use platform::{scope_list, scope_name};
pub use platform::{
    CredentialRecord, Grant, ProjectDefault, CREDENTIAL_REVOKED_EVENT, PLATFORM_CONNECTED_EVENT,
    PLATFORM_DEFAULT_EVENT, PLATFORM_DISCONNECTED_EVENT, PLATFORM_STREAM, SCOPE_GRANTED_EVENT,
    SCOPE_REVOKED_EVENT,
};
mod effects;
pub use effects::{
    presser_json, DraftRow, EffectKey, EffectRow, EFFECT_CANCELLED_EVENT, EFFECT_DECIDED_EVENT,
    EFFECT_EXECUTED_EVENT, EFFECT_FAILED_EVENT, EFFECT_NEEDS_YOU_EVENT, EFFECT_REQUESTED_EVENT,
};
mod threads;
pub use threads::{
    tool_result_summary, tool_summary, NewEntry, Sender, Thread, ThreadEntry, KIND_ASSISTANT_TEXT,
    KIND_MESSAGE, KIND_TOOL_CALL, KIND_TOOL_RESULT, KIND_TURN_RESULT, PAGE_MAX as THREAD_PAGE_MAX,
    ROLE_AGENT, ROLE_OPERATOR, ROLE_SYSTEM,
};
pub use threads::{COMPACTED_EVENT as THREAD_COMPACTED_EVENT, PACK_EVENT as THREAD_PACK_EVENT};

mod agents;
pub use agents::{Agent, ModelDefaultsSnapshot, NewAgent, AGENT_STATES};
mod delivery;
mod events;
pub use events::{
    default_approval_id, Event, NewApproval, APPROVAL_RECORDED_EVENT, APPROVAL_REVOKED_EVENT,
    APPROVAL_STREAM, APP_APPROVED_EVENT, DELIVERY_ROLLUP_EVENT, EVENT_ROLLUP_AGE_SECS,
    EVENT_ROLLUP_BATCH, ROLLABLE_EVENT_KINDS, VERDICT_RECORDED_EVENT, VERDICT_STREAM,
    WORKFLOW_APPROVED_EVENT, WORK_APPROVED_EVENT,
};
// CAD-316: the rollup's read-only counts for `doctor --host`.
pub(crate) use events::event_store_stats;
mod inbox;
mod kickoff;
pub use kickoff::{check_commit_sha, job_kickoff, kickoff_ceiling, omit_host_paths};
mod messages;
pub use messages::{
    report_timeout_secs, Message, Priority, Steer, DEFAULT_REPORT_TIMEOUT_SECS, ENQUEUE_BYTES,
    NUDGE_SOURCE,
};
mod monitors;
pub use monitors::{Monitor, MonitorAlert, MonitorCheck};
mod plans;
pub use plans::{current_verdict, Job, Task, Verdict, JOB_STATES};
mod quota;
mod schema;
pub(crate) use schema::open_read_only;
pub use schema::{AdoptEntry, ConsumedMarker, Take};
#[cfg(test)]
mod tests;

/// How long a connection waits on another process's lock before
/// SQLITE_BUSY (CAD-256). The daemon's writer and every out-of-process
/// reader (`audit`, `issue retro`, `doctor host`) share one WAL store.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub struct Store {
    conn: Mutex<Connection>,
    /// CAD-538: the hosted-lease write fence — installed by the daemon
    /// when it runs under `hosted.lease`. `write_conn` refuses once it
    /// trips; reads stay up so a fenced daemon can still be diagnosed.
    write_fence: std::sync::OnceLock<Arc<crate::lease::Fence>>,
    /// Adoption candidates that survived `recover()`'s store-level
    /// checks, keyed by alias — an agent may hold more than one
    /// in-flight turn (routed notifications beside its one report-owing
    /// turn, or rows that accumulated before CAD-250, which the report
    /// bound then retires), so every qualifying entry is kept; each list is
    /// consumed exactly once by the agent's actor at open
    /// (`take_adoption`).
    adoptions: Mutex<std::collections::HashMap<String, Vec<AdoptEntry>>>,
    /// Provider text the turn result may carry, held per running message
    /// until it finishes ([`Store::thread_hold_running`], CAD-320).
    thread_held: Mutex<std::collections::HashMap<String, Vec<threads::HeldText>>>,
}

/// Terminal task states — verdicts/acceptance/cancellation are closed
/// to these. `verified` sits between review and done (accept pending).
fn is_task_terminal(state: &str) -> bool {
    matches!(state, "verified" | "done" | "cancelled" | "failed")
}

/// Terminal message states — same set `daemon::is_terminal` uses;
/// duplicated here so store code doesn't reach into the daemon module.
fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "interrupted" | "unknown" | "cancelled"
    )
}

fn take_bytes(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

impl Store {
    /// The only way to take the connection lock (CAD-256). A panic while
    /// another caller held the guard poisons the mutex; `lock().unwrap()`
    /// would then panic on every later call and take the daemon down
    /// with it. The connection itself is still sound — an unwinding
    /// `Transaction` rolls back on drop — so recover the guard, clear
    /// the poison, roll back anything a raw `BEGIN` left open, and
    /// record one `store_poisoned` event on the daemon stream.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        match self.conn.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let guard = poisoned.into_inner();
                self.conn.clear_poison();
                let rolled_back = !guard.is_autocommit();
                if rolled_back {
                    let _ = guard.execute_batch("ROLLBACK");
                }
                eprintln!(
                    "store: connection lock was poisoned by a panic; recovered \
                     (rolled_back={rolled_back})"
                );
                // CAD-538: a fenced daemon writes nothing — not even the
                // forensic row for the poison it just recovered.
                let fenced = self.write_fence.get().is_some_and(|f| f.check().is_some());
                if !fenced {
                    if let Err(e) = Self::event(
                        &guard,
                        Self::DAEMON_STREAM,
                        "store_poisoned",
                        json!({"rolled_back": rolled_back}),
                    ) {
                        eprintln!("store: could not record store_poisoned: {e}");
                    }
                }
                guard
            }
        }
    }

    /// CAD-538: the write path's connection — [`Self::conn`] plus the
    /// hosted-lease fence, checked while holding the lock so the check
    /// and the write that follows it are serialized against the trip.
    /// `check` covers both halves of lease loss: the detected trip and
    /// the held lease's expiry — a shutdown tail outliving the TTL
    /// cannot commit into a lease a successor already took. The
    /// heartbeat's [`Self::fence_writes`] drains the in-flight writer
    /// before it returns, so no write starts post-trip.
    fn write_conn(&self) -> Result<MutexGuard<'_, Connection>> {
        let guard = self.conn();
        if let Some(reason) = self.write_fence.get().and_then(|f| f.check()) {
            return Err(Error::rejected(format!(
                "store write refused — the daemon's hosted lease is lost: {reason}"
            )));
        }
        Ok(guard)
    }

    /// Install the hosted-lease fence — the daemon calls this right
    /// after open, only when a lease was acquired.
    pub fn install_write_fence(&self, fence: Arc<crate::lease::Fence>) {
        let _ = self.write_fence.set(fence);
    }

    /// Trip the fence, then wait out the writer in flight — after this
    /// returns, every [`Self::write_conn`] observes the trip before its
    /// write begins.
    pub fn fence_writes(&self, reason: impl Into<String>) {
        if let Some(fence) = self.write_fence.get() {
            fence.trip(reason);
        }
        let _guard = self.conn();
    }

    /// Why writes are fenced, when they are (`health` and tests).
    pub fn fence_reason(&self) -> Option<String> {
        self.write_fence.get().and_then(|f| f.check())
    }

    /// CAD-538: fold the WAL back into the db file — the SIGTERM
    /// flush's SQLite half. PASSIVE first (never blocks), then TRUNCATE;
    /// a foreign reader keeps the file alive, which is `Ok(false)`,
    /// not a failure — the WAL is durable either way, just not merged.
    pub fn checkpoint(&self) -> Result<bool> {
        let conn = self.conn();
        let _ = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()));
        match conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        }) {
            Ok((0, log)) => Ok(log >= 0),
            _ => Ok(false),
        }
    }
}
