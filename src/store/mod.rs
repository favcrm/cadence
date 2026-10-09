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
    tool_result_summary, tool_summary, ConversationKind, NewEntry, Sender, Thread, ThreadEntry,
    CONVERSATION_LIMIT, KIND_ASSISTANT_TEXT, KIND_MESSAGE, KIND_TOOL_CALL, KIND_TOOL_RESULT,
    KIND_TURN_RESULT, PAGE_MAX as THREAD_PAGE_MAX, ROLE_AGENT, ROLE_OPERATOR, ROLE_SYSTEM,
    SUBJECT_CAMPAIGN,
};
pub use threads::{COMPACTED_EVENT as THREAD_COMPACTED_EVENT, PACK_EVENT as THREAD_PACK_EVENT};

mod agents;
mod cloud_dispatch_outbox;
pub use agents::{Agent, ModelDefaultsSnapshot, NewAgent, AGENT_STATES};
mod delivery;
mod events;
pub use events::{
    board_revocable, default_approval_id, Event, NewApproval, NewDelegated,
    APPROVAL_RECORDED_EVENT, APPROVAL_RECORDED_VIA, APPROVAL_REVOKED_EVENT, APPROVAL_STREAM,
    APP_APPROVED_EVENT, AUTO_RESUME_EVENT, AUTO_RESUME_FAILED_EVENT, AUTO_STOP_EVENT,
    AUTO_STOP_MARKER_KINDS, DELIVERY_ROLLUP_EVENT, DESIGNATION_EVENT, EVENT_ROLLUP_AGE_SECS,
    EVENT_ROLLUP_BATCH, ROLLABLE_EVENT_KINDS, VERDICT_RECORDED_EVENT, VERDICT_STREAM,
    WORKFLOW_APPROVED_EVENT, WORK_APPROVED_EVENT,
};
// CAD-316: the rollup's read-only counts for `doctor --host`.
pub(crate) use events::event_store_stats;
mod inbox;
mod kickoff;
pub use kickoff::{check_commit_sha, job_kickoff, kickoff_ceiling, omit_host_paths};
mod messages;
#[cfg(test)]
pub(crate) use messages::take_decoded_messages;
pub use messages::{
    report_timeout_secs, Message, Priority, Steer, DEFAULT_REPORT_TIMEOUT_SECS, ENQUEUE_BYTES,
    NUDGE_SOURCE,
};
mod monitors;
pub use monitors::{Monitor, MonitorAlert, MonitorCheck};
mod chat_files;
pub use chat_files::{
    text_kind as chat_text_kind, valid_id as chat_file_id, ChatFile, ChatFileStorageRoots,
    CHAT_FILE_MAX_BYTES, CHAT_FILE_MAX_PER_MESSAGE, CHAT_FILE_SCOPE_HOME, CHAT_FILE_TEXT_CAP,
    CHAT_QUOTA_CODE,
};
pub mod app_audiences;
pub use app_audiences::{AudienceBase, Predicate};
pub mod app_content;
pub mod app_content_html;
pub use app_content::{Block, Draft};
pub mod app_bindings;
pub mod app_capabilities;
pub mod app_contexts;
pub mod app_effects;
pub mod app_explorer;
pub mod app_records;
pub mod app_runs;
pub mod app_sends;
pub mod app_social_drafts;
pub mod app_tools;
pub mod crm_sends;
pub mod crm_smtp;
mod plans;
pub mod social_publish;
pub use plans::{current_verdict, AssigneeTask, Job, Task, Verdict, JOB_STATES};
mod quota;
mod schema;
mod seal;
pub(crate) use schema::open_read_only;
pub use schema::{AdoptEntry, ConsumedMarker, RecoveryOutcome, Take};
pub use seal::OpenMode;
// `WriteTxn` is `pub` so the `ShutdownEntriesHook` test seam (a `pub`
// `ServeOptions` field) can name it — it is an opaque facade to external
// callers: every method is `pub(crate)` except the read/DML verbs
// `execute`/`execute_batch`/`query_row`/`query_map` a hook legitimately
// needs, and there is no `commit`/`rollback`/`Connection` escape.
pub use seal::WriteTxn;
pub(crate) use seal::{preflight_writer_guard, require_legacy_writer_tx, StoreConn};
#[cfg(test)]
mod tests;
#[cfg(test)]
mod writer_census;

/// How long a connection waits on another process's lock before
/// SQLITE_BUSY (CAD-256). The daemon's writer and every out-of-process
/// reader (`audit`, `issue retro`, `doctor host`) share one WAL store.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub struct Store {
    conn: Mutex<Connection>,
    /// CAD-538: the hosted-lease write fence — installed by the daemon
    /// when it runs under `hosted.lease`. `write_tx` refuses once it
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
    /// Test seam (CAD-694): invoked inside every `shutdown_entries`
    /// transaction with that attempt's live [`WriteTxn`] — a test can
    /// mutate rows or return a synthetic sqlite error to prove rollback
    /// and retry. The hook is NOT producer authority: it receives only
    /// the restricted `WriteTxn` facade (no `commit`/`rollback`/`Connection`
    /// escape). Production leaves it unset; a protected-mode store
    /// refuses registration (see [`Self::set_shutdown_entries_hook`]).
    /// Never set directly — registration goes through the setter so the
    /// protected-mode refusal applies.
    shutdown_entries_hook: Option<ShutdownEntriesHook>,
    /// CAD-1011: true only when the store was opened under
    /// `OpenMode::Protected`. A protected open is unreachable today
    /// (`preflight` refuses it), so this is always false — recorded so a
    /// future protected path fails closed: `set_shutdown_entries_hook`
    /// and `shutdown_entries`' hook execution refuse when set.
    protected_open: bool,
    /// Test seam (CAD-694): the `shutdown_entries` retry backoff
    /// multiplier in milliseconds — production 50; tests set 0 so the
    /// retry bound is proven without wall-clock sleeps.
    pub(crate) shutdown_backoff_ms: u64,
    /// CAD-1011: the producer-closure guard state shared with this
    /// connection's SQLite authorizer — arm level + tx-control phase,
    /// driven under `conn`'s mutex so a prepared write can never step
    /// outside the armed window.
    seal_state: std::sync::Arc<seal::GuardState>,
    /// CAD-1011: this store's bound identity — the canonicalized database
    /// path recorded at open. `OwnerMaintenancePermit::database_id` must
    /// equal it, so a permit issued for one store can never authorize
    /// owner maintenance on another. `pub(super)` — the owner-op bodies
    /// in `seal.rs` read it; it is a local identity only, never
    /// authenticated restore/incarnation provenance.
    pub(super) db_identity: String,
}

/// The `shutdown_entries` test seam (CAD-694): called with each
/// attempt's live transaction; the hook may write through it or return
/// a synthetic sqlite error, so rollback, retry bound and
/// retryable-classification are provable without wedging the store.
// The test-seam hook type. `WriteTxn` is `pub` but opaque — every
// method on it is `pub(crate)`, so the public facade exposes nothing
// usable (no prepare/execute/Connection escape) to an outside caller.
pub type ShutdownEntriesHook = Arc<dyn Fn(&WriteTxn<'_>) -> rusqlite::Result<()> + Send + Sync>;

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
    /// CAD-1212 test seam: rows inserted, updated or deleted on the daemon
    /// store connection since it opened (SQLite `total_changes`). A check
    /// reads it before and after a call and asserts it did not move, to
    /// prove a callback switched to the lock-free
    /// `workspace::with_runtime_read` writes nothing. Compiled out of release.
    #[cfg(test)]
    pub(crate) fn total_changes_for_test(&self) -> u64 {
        self.conn().total_changes()
    }

    /// The only way to take the connection lock (CAD-256). A panic while
    /// another caller held the guard poisons the mutex; `lock().unwrap()`
    /// would then panic on every later call and take the daemon down
    /// with it. Recover the guard, verify rollback, and attempt one
    /// `store_poisoned` event on the daemon stream. Clear poison only
    /// after forensic cleanup is verified; an unverified connection
    /// remains poisoned and unavailable rather than leaking a writer.
    ///
    /// The returned guard is DISARMED — the authorizer denies every
    /// write/DML/DDL/tx-boundary it attempts, so test/fixture read
    /// probes (`s.conn().query_row`) are safe: this is a read surface,
    /// never a producer path. Private to `store` — test children under
    /// `store::tests` still reach it as a private ancestor member.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        match self.conn.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let guard = poisoned.into_inner();
                // CAD-1011: VERIFY the rollback, don't assume it. A panic
                // can leave the conn inside a tx *or* in autocommit —
                // `!is_autocommit()` alone is not proof a rollback ran.
                // `verified_rollback` runs ROLLBACK inside the owner's
                // TxControl window (the disarmed authorizer would refuse
                // it) and reports `RolledBack` ONLY once the conn is
                // actually back in autocommit; a denied/failed ROLLBACK
                // is `Unverified`, never silently labelled rolled back.
                let recovery = Self::verified_rollback(&self.seal_state, &guard);
                let state_label = match recovery {
                    seal::PoisonRecovery::CleanAutocommit => "clean_autocommit",
                    seal::PoisonRecovery::RolledBack => "rolled_back",
                    seal::PoisonRecovery::Unverified => "unverified",
                };
                eprintln!(
                    "store: connection lock was poisoned by a panic; recovered \
                     (state={state_label})"
                );
                // CAD-538 + CAD-1011: a fenced daemon writes nothing —
                // not even the forensic row — and a poison whose rollback
                // is UNVERIFIED records no forensic write at all: the
                // connection may still be inside a transaction, so a
                // write now would be attributed to the wrong tx or fail
                // mislabeled. Only a known-clean conn (already autocommit,
                // or a verified rollback) gets the forensic row.
                let clean = !matches!(recovery, seal::PoisonRecovery::Unverified);
                assert!(
                    clean,
                    "store: poison recovery is unverified; connection remains unavailable"
                );
                let fenced = self.write_fence.get().is_some_and(|f| f.check().is_some());
                if !fenced {
                    // CAD-1011: the forensic recovery event is an
                    // owner-maintenance write inside ONE held BEGIN
                    // IMMEDIATE — the closure-latch re-check and the row
                    // are one critical section, and only an explicit safe
                    // unsealed outcome (LatchAbsent/LatchOpen) may write.
                    // A sealed, malformed or unreadable latch records
                    // nothing — never a false `rolled_back` on a closed
                    // or unknown store.
                    let rolled_back = matches!(recovery, seal::PoisonRecovery::RolledBack);
                    assert!(
                        Self::forensic_poison_event(
                            &self.seal_state,
                            &guard,
                            rolled_back,
                            state_label
                        ),
                        "store: forensic cleanup is unverified; connection remains unavailable"
                    );
                }
                assert!(
                    guard.is_autocommit(),
                    "store: recovery retained an open transaction"
                );
                self.conn.clear_poison();
                guard
            }
        }
    }

    pub(crate) fn try_conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .try_lock()
            .map_err(|_| Error::busy("store is busy"))
    }

    /// CAD-1011: the ONLY producer write lane. `f` runs inside
    /// `BEGIN IMMEDIATE` on the held conn mutex after the durable
    /// closure latch and the hosted-lease fence are both re-checked
    /// *inside* the lock — closure-check, lease-check and every DML are
    /// one SQLite writer critical section. A fenced or sealed store
    /// refuses before `f` runs; a callback error/panic rolls back.
    ///
    /// `check` covers both halves of lease loss: the detected trip and
    /// the held lease's expiry — a shutdown tail outliving the TTL
    /// cannot commit into a lease a successor already took. The
    /// heartbeat's [`Self::fence_writes`] drains the in-flight writer
    /// before it returns, so no write starts post-trip.
    pub(crate) fn write_tx<R>(
        &self,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> Result<R>,
    ) -> Result<R>
    where
        R: 'static,
    {
        // The fence is re-checked inside the held conn mutex by
        // `with_sealed_tx_fenced` — a writer fenced while waiting on the
        // lock is refused before arming or opening the tx.
        self.with_sealed_tx_fenced(|| self.write_fence.get().and_then(|f| f.check()), f)
    }

    /// Raw-error variant of [`Self::write_tx`] — the callback returns
    /// `rusqlite::Result` so callers classify BUSY/constraint at the
    /// source (e.g. `shutdown_entries`' typed-retry path).
    pub(crate) fn write_tx_raw<T>(
        &self,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T>
    where
        T: 'static,
    {
        self.with_sealed_tx_fenced_raw(|| self.write_fence.get().and_then(|f| f.check()), f)
    }

    /// Install the hosted-lease fence — the daemon calls this right
    /// after open, only when a lease was acquired.
    pub fn install_write_fence(&self, fence: Arc<crate::lease::Fence>) {
        let _ = self.write_fence.set(fence);
    }

    /// Register the CAD-694 `shutdown_entries` test hook. An arbitrary
    /// hook is not producer authority — it runs inside the sealed write
    /// tx on the restricted `WriteTxn` facade only. Refuses on a
    /// protected-mode store: a protected db's maintenance authority is
    /// external, and a daemon-side hook must never stand in for it.
    ///
    /// A `Protected` open is unreachable today (`preflight` returns
    /// `Unknown`), so `protected_open` is always false — the refusal is
    /// written so the hook path fails closed the day a protected open
    /// becomes reachable, not because one exists now.
    pub(crate) fn set_shutdown_entries_hook(
        &mut self,
        hook: Option<ShutdownEntriesHook>,
    ) -> Result<()> {
        if self.protected_open && hook.is_some() {
            return Err(Error::rejected(
                "shutdown_entries hook cannot be registered on a \
                 protected-mode store",
            ));
        }
        self.shutdown_entries_hook = hook;
        Ok(())
    }

    /// Trip the fence, then wait out the writer in flight — after this
    /// returns, every [`Self::write_tx`] observes the trip before its
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
        self.owner_checkpoint()
    }
}
