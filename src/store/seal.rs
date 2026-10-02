//! Durable producer-closure latch + owner witness barrier (CAD-1011).
//!
//! One `cadence.sqlite3` writer gate: every mutation runs inside
//! `BEGIN IMMEDIATE` after a durable `closure_state` check evaluated *in*
//! the same transaction, so a queued writer either commits before the
//! barrier or sees `closed` and rolls back — no check-then-act gap, and the
//! check serializes at the file level against every connection (the
//! daemon's `Store`, `open_side`, and `rollout` writers).
//!
//! Callers get a `WriteTxn` facade — never the `Transaction`/`Connection` —
//! so a business callback cannot run `COMMIT`/`BEGIN`/`SAVEPOINT`/DDL,
//! cannot touch the latch/witness rows (those need the `Owner` arm), and
//! cannot return a borrowed `Statement`. The authorizer is the structural
//! deny that a prepared statement can never bypass (it fires at PREPARE);
//! the facade + `'t`/`R: 'static` bounds keep a prepared write inside the
//! armed window.
//!
//! `OpenMode::Protected` is a *request* from already-authenticated daemon
//! startup provenance (`agent_uid`/`hosted.lease` `ServeOptions`) — it
//! forces the strict refusals, it never grants restore/init eligibility.
//! Without separately-proven external pre-start provenance the production
//! protected constructor is unreachable: a real protected open is
//! `Err(Unknown)`; only `#[cfg(test)]`/fixture construction reaches a
//! guarded protected connection. No runtime or provisioning `InitPermit`
//! factory exists.

use crate::error::{Error, Result};
use rusqlite::hooks::{AuthAction, Authorization};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use super::Store;

/// Highest reviewed schema the guard understands. A newer/unknown
/// protected schema is `Unknown`, never default-open.
#[allow(dead_code)]
const PROTECTED_SCHEMA_MAX: i64 = 32;

/// Arm level + transaction-control phase, shared between the `Store` and
/// the connection's authorizer. Atomic because the authorizer callback must
/// be `Send`; the conn mutex already serializes every transition, so the
/// atomics are only for `Send`-safety, never a second writer.
#[derive(Default)]
pub(super) struct GuardState {
    /// 0 = Disarmed (default; all writes denied), 1 = Business, 2 = Owner.
    arm: AtomicU8,
    /// 0 = Callback (deny tx-boundary), 1 = TxControl (guard's own
    /// BEGIN/COMMIT/ROLLBACK/SAVEPOINT window).
    phase: AtomicU8,
}
impl GuardState {
    const DISARMED: u8 = 0;
    const BUSINESS: u8 = 1;
    const OWNER: u8 = 2;
    const CALLBACK: u8 = 0;
    const TX_CONTROL: u8 = 1;
}

/// The finite owner-maintenance operation a permit authorizes. A permit
/// minted for `Close` can never authorize `Witness` and vice versa — the
/// discriminant is a private field checked at the barrier, so separate
/// one-use permits cannot be repurposed across operations.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnerOp {
    /// `propose_close` — flip the durable closure latch.
    Close,
    /// `witness_commit` — the sealed-store witness barrier.
    Witness,
}

/// A single-use, externally-issued authority to run ONE owner
/// maintenance operation on *this* database. The permit is bound to the
/// database's identity, the closure challenge, the operator's close
/// attempt, the produced artifact identity, the latch epoch and a
/// wall-clock deadline — every field is read back and compared against
/// the durable latch inside the sealed tx before the write commits.
///
/// Forgery resistance: the type is opaque — its fields are private and
/// there is no setter, `Default`, `Clone` or `Debug` deriving that would
/// let a caller reshape a permit mid-flight. `consumed` makes the
/// barrier one-use: `take()` flips the flag once; a second maintenance
/// call on the same permit refuses. A caller can never mint one — only
/// `OwnerMaintenancePermit::issue` (authenticated external authority,
/// unreachable in this build) and the `#[cfg(test)]` synthetic factory
/// produce one.
pub(super) struct OwnerMaintenancePermit {
    /// The operation this permit authorizes — `Close` vs `Witness`; a
    /// permit issued for one operation refuses the other.
    op: OwnerOp,
    /// The database this permit authorizes — bound to the *recorded local
    /// identity* (the canonicalized path at open), so a permit minted for
    /// one store cannot authorize maintenance on another. This is a local
    /// identity binding only — it is NOT authenticated restore/incarnation
    /// provenance (no external authority exists to supply one).
    database_id: String,
    /// The closure challenge the sealed latch recorded.
    challenge: Vec<u8>,
    /// The operator's close-attempt identifier.
    attempt: String,
    /// The produced artifact identity the witness attests to.
    artifact: String,
    /// The latch epoch the closure was issued under.
    epoch: u64,
    /// Unix-deadline the permit expires at — a stale permit refuses.
    deadline_unix: i64,
    /// One-use barrier — flipped by `take()`; a spent permit refuses.
    consumed: std::sync::atomic::AtomicBool,
}

impl OwnerMaintenancePermit {
    /// The production authority path. There is no authenticated external
    /// authority wired in this build, so issuing a permit always fails —
    /// a caller cannot conjure owner authority locally.
    ///
    /// This is deliberately not `dead_code`-gated: the production `Err`
    /// is the *correct* runtime behavior, and keeping it reachable keeps
    /// the signature honest for the future authority wiring (CAD-1011's
    /// final upload/public-RPC authority is explicitly out of scope).
    #[allow(dead_code)]
    pub(super) fn issue(
        _database_id: &str,
        _op: OwnerOp,
        _challenge: &[u8],
        _attempt: &str,
        _artifact: &str,
        _epoch: u64,
        _deadline_unix: i64,
    ) -> Result<Self> {
        Err(Error::rejected(
            "no authenticated external owner-maintenance authority is \
             reachable — a production OwnerMaintenancePermit cannot be issued",
        ))
    }

    /// Test-only synthetic factory — `#[cfg(test)]` so a release build
    /// can never mint one even under `test-seam`. Exists purely so unit
    /// tests exercise the owner-maintenance barrier.
    #[cfg(test)]
    pub(super) fn synthetic(
        database_id: &str,
        op: OwnerOp,
        challenge: &[u8],
        attempt: &str,
        artifact: &str,
        epoch: u64,
        deadline_unix: i64,
    ) -> Self {
        Self {
            op,
            database_id: database_id.to_string(),
            challenge: challenge.to_vec(),
            attempt: attempt.to_string(),
            artifact: artifact.to_string(),
            epoch,
            deadline_unix,
            consumed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Consume the one-use barrier for `expected` operation — refuses a
    /// permit minted for a different operation, a stale deadline, or a
    /// second use. `Ok` flips `consumed` exactly once.
    fn take(&self, expected: OwnerOp) -> Result<()> {
        if self.op != expected {
            return Err(Error::rejected(
                "owner-maintenance permit authorizes a different operation",
            ));
        }
        if self.deadline_unix <= super::now() as i64 {
            return Err(Error::rejected(
                "owner-maintenance permit is past its deadline",
            ));
        }
        if self
            .consumed
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(Error::rejected(
                "owner-maintenance permit is already consumed",
            ));
        }
        Ok(())
    }

    /// The recorded local identity this permit is bound to.
    fn database_id(&self) -> &str {
        &self.database_id
    }
}

/// RAII: set the arm level for the duration of one guarded tx; `Drop`
/// restores `Disarmed`. The write window cannot outlive its tx.
struct ArmGuard<'a> {
    state: &'a GuardState,
}
impl<'a> ArmGuard<'a> {
    fn enter(state: &'a GuardState, level: u8) -> Self {
        state.arm.store(level, Ordering::SeqCst);
        Self { state }
    }
}
impl Drop for ArmGuard<'_> {
    fn drop(&mut self) {
        self.state.arm.store(GuardState::DISARMED, Ordering::SeqCst);
    }
}

/// RAII: set `TxControl` only around the guard's own boundary statements;
/// `Drop` restores `Callback`. Always `Callback` while `f` runs.
struct ControlPhase<'a> {
    state: &'a GuardState,
}
impl<'a> ControlPhase<'a> {
    fn enter(state: &'a GuardState) -> Self {
        state.phase.store(GuardState::TX_CONTROL, Ordering::SeqCst);
        Self { state }
    }
}
impl Drop for ControlPhase<'_> {
    fn drop(&mut self) {
        self.state
            .phase
            .store(GuardState::CALLBACK, Ordering::SeqCst);
    }
}

/// Constructor/maintenance scope: run `f` with `arm=Owner` +
/// `phase=TxControl` for the duration — the only scope in which DDL,
/// migration DML and tx-boundary statements are authorized. Internal to
/// `open_inner`/`propose_close`/`witness_commit`; a caller can never mint
/// `Owner`. RAII restores both on drop (panic-safe).
/// The owner arming window — `store`-module-internal (pub(super)) so the
/// finite owner-maintenance ops (schema bootstrap, WAL conversion,
/// record/enforce/ingest, propose_close, witness_commit) and the conn()
/// poison-recovery write may arm `Owner`+`TxControl` — never a business
/// caller surface, never `pub(crate)`.
pub(super) fn with_owner_tx_control<R>(state: &GuardState, f: impl FnOnce() -> R) -> R {
    let _a = ArmGuard::enter(state, GuardState::OWNER);
    let _c = ControlPhase::enter(state);
    f()
}

/// SQLite built-ins the store's business SQL actually uses — enumerated
/// from `src/store/**`. Everything else, and `load_extension`, is denied.
const ALLOWED_FUNCTIONS: &[&str] = &[
    "coalesce",
    "ifnull",
    "nullif",
    "count",
    "max",
    "min",
    "sum",
    "total",
    "abs",
    "length",
    "substr",
    "replace",
    "lower",
    "upper",
    "hex",
    "typeof",
    "instr",
    "strftime",
    "datetime",
    "date",
    "time",
    "julianday",
    "unixepoch",
    "round",
    "printf",
    "glob",
    "group_concat",
    "json_extract",
    "json_array",
    "json_object",
    "json_array_length",
    "json_type",
    "json_valid",
    "json_quote",
    "json_remove",
    "json_set",
    "json_insert",
    "changes",
    "last_insert_rowid",
    "random",
    "randomblob",
    "trim",
    "ltrim",
    "rtrim",
    "char",
    "unicode",
    "avg",
];

/// Functions that must never be reachable from a business or owner
/// statement — extension-load, filesystem, tokenizer and sqlite-version
/// introspection surface. The Function arm already denies anything not in
/// `ALLOWED_FUNCTIONS`; this list makes the dangerous set explicit so the
/// adversarial test can assert each name is refused.
const DENIED_FUNCTIONS: &[&str] = &[
    "load_extension",
    "fts3_tokenizer",
    "fts5_api",
    "readfile",
    "writefile",
    "sqlite_compileoption_get",
    "sqlite_compileoption_used",
    "sqlite_offset",
    "sqlite_source_id",
    "sqlite_version",
    "zipfile",
];

/// PRAGMAs that read schema metadata and legitimately take a table
/// argument — `PRAGMA table_info(x)` is a read, not a mutation. Allowed
/// in the business lane with or without the argument.
const QUERY_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "integrity_check",
    "quick_check",
    "compile_options",
    "data_version",
];

/// PRAGMAs allowed in the business lane ONLY in their argument-free read
/// form (`PRAGMA name` — `pragma_value: None`). A value form is a write
/// (`journal_mode=WAL`, `user_version=9`, `application_id=…`) and must
/// not pass. `wal_checkpoint`, `optimize`, `busy_timeout`, `synchronous`,
/// `foreign_keys=`, `cache_size=` and every other connection-state or
/// mutating pragma are in neither list — a business statement never
/// alters connection state, checkpoints or schema; a checkpoint is an
/// explicit owner-lane operation.
const READ_PRAGMAS: &[&str] = &[
    "journal_mode",
    "user_version",
    "schema_version",
    "page_count",
    "page_size",
    "application_id",
    "database_list",
    "foreign_key_check",
    "busy_timeout",
    "synchronous",
    "foreign_keys",
    "recursive_triggers",
    "cache_size",
    "mmap_size",
    "auto_vacuum",
    "encoding",
    "locking_mode",
    "max_page_count",
    "defer_foreign_keys",
    "query_only",
];

/// The fail-closed SQLite authorizer, installed on every writable
/// `cadence.sqlite3` connection. Fires at PREPARE; combined with the
/// no-escape bounds it makes a prepared write unable to survive the armed
/// window. `Arc<GuardState>` is shared with the `Store`, so the same cells
/// the conn mutex drives feed the callback.
pub(super) fn install_authorizer(conn: &Connection, state: Arc<GuardState>) {
    conn.set_prepared_statement_cache_capacity(0);
    conn.authorizer(Some(move |ctx: rusqlite::hooks::AuthContext<'_>| {
        let armed = state.arm.load(Ordering::SeqCst);
        let tx_ctrl = state.phase.load(Ordering::SeqCst) == GuardState::TX_CONTROL;
        match ctx.action {
            // Transaction-boundary/savepoint actions are allowed ONLY in
            // the guard's private `TxControl` window — the WriteTxn facade
            // can't reach them, and a business or owner callback that issues
            // `COMMIT`/`BEGIN`/`SAVEPOINT`/`ROLLBACK` while the phase is
            // `Callback` is denied. This is the early-commit / tx-escape
            // denial; the name of a savepoint is never consulted.
            AuthAction::Transaction { .. } | AuthAction::Savepoint { .. } => {
                if tx_ctrl {
                    Authorization::Allow
                } else {
                    Authorization::Deny
                }
            }
            // DDL / attach / schema escape / analysis — owner-maintenance
            // (migrations) only, and only inside the guard's tx-control
            // window; never the business lane.
            AuthAction::CreateIndex { .. }
            | AuthAction::CreateTable { .. }
            | AuthAction::CreateTempIndex { .. }
            | AuthAction::CreateTempTable { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::CreateTrigger { .. }
            | AuthAction::CreateView { .. }
            | AuthAction::DropIndex { .. }
            | AuthAction::DropTable { .. }
            | AuthAction::DropTempIndex { .. }
            | AuthAction::DropTempTable { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::DropTrigger { .. }
            | AuthAction::DropView { .. }
            | AuthAction::AlterTable { .. }
            | AuthAction::Reindex { .. }
            | AuthAction::Analyze { .. }
            | AuthAction::CreateVtable { .. }
            | AuthAction::DropVtable { .. }
            | AuthAction::Attach { .. }
            | AuthAction::Detach { .. } => {
                // Owner-maintenance only; the owner arm is the authority,
                // not the tx-control phase (which gates BEGIN/COMMIT/
                // SAVEPOINT, not the DML/DDL inside the tx).
                if armed == GuardState::OWNER {
                    Authorization::Allow
                } else {
                    Authorization::Deny
                }
            }
            // The latch/witness rows are owner-maintenance only — a
            // business writer can never un-seal or forge a witness.
            AuthAction::Insert { table_name }
            | AuthAction::Update { table_name, .. }
            | AuthAction::Delete { table_name }
                if table_name == "closure_state" || table_name == "owner_witness" =>
            {
                if armed == GuardState::OWNER {
                    Authorization::Allow
                } else {
                    Authorization::Deny
                }
            }
            AuthAction::Insert { .. } | AuthAction::Update { .. } | AuthAction::Delete { .. } => {
                if armed == GuardState::DISARMED {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }
            AuthAction::Pragma {
                pragma_name,
                pragma_value,
            } => {
                // Business lane: only an argument-free read of a named
                // read-pragma passes. `PRAGMA optimize` (value-less but
                // mutating), `wal_checkpoint(TRUNCATE)`, `journal_mode=WAL`,
                // `busy_timeout`, `synchronous`, `foreign_keys=` and every
                // other value/arg form are denied — a business statement
                // never alters connection state, checkpoints or schema.
                // The owner lane may run pragma reads/writes for
                // maintenance (a checkpoint is its explicit operation).
                if armed == GuardState::OWNER
                    || QUERY_PRAGMAS.contains(&pragma_name)
                    || (pragma_value.is_none() && READ_PRAGMAS.contains(&pragma_name))
                {
                    Authorization::Allow
                } else {
                    Authorization::Deny
                }
            }
            AuthAction::Function { function_name } => {
                // Function names arrive in the caller's case; compare
                // case-insensitively. Dangerous extension/load/fs functions
                // are denied in every lane (owner included); the business
                // lane is restricted to the finite allowlist.
                let fname = function_name.to_ascii_lowercase();
                if DENIED_FUNCTIONS.contains(&fname.as_str()) {
                    Authorization::Deny
                } else if armed == GuardState::OWNER || ALLOWED_FUNCTIONS.contains(&fname.as_str())
                {
                    Authorization::Allow
                } else {
                    Authorization::Deny
                }
            }
            // Explicitly safe read actions — nothing else falls through.
            AuthAction::Read { .. } | AuthAction::Select | AuthAction::Recursive => {
                Authorization::Allow
            }
            // Deny-default: any current or future AuthAction variant not
            // named above is refused.
            _ => Authorization::Deny,
        }
    }));
}

/// The durable latch + witness schema — owner-maintenance writes only.
/// `closure_state` is one row (`id=1`); `owner_witness.seq` is the owner
/// barrier sequence, distinct from any business last-commit highwater.
#[allow(dead_code)]
pub(super) const SEAL_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS closure_state(
    id INTEGER PRIMARY KEY CHECK(id=1),
    closed INTEGER NOT NULL,
    reason TEXT,
    challenge BLOB,
    attempt TEXT,
    artifact TEXT,
    epoch INTEGER,
    witness_done INTEGER NOT NULL DEFAULT 0,
    closed_at REAL);
CREATE TABLE IF NOT EXISTS owner_witness(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    challenge BLOB NOT NULL,
    attempt TEXT NOT NULL,
    artifact_identity TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    db_schema INTEGER NOT NULL,
    committed_at REAL NOT NULL);
";

/// What the read-only preflight sees. The latch is refusal evidence, never
/// authority to *be* protected.
#[derive(Debug, PartialEq, Clone, Copy)]
pub(super) enum Preflight {
    /// No `closure_state` table — a pre-mechanism db.
    LatchAbsent,
    /// `closed=0`, well-formed — a protected-latch db, still open.
    LatchOpen,
    /// `closed=1` — sealed; refuse writable open in every mode.
    Sealed,
    /// `closure_state` present but unreadable/`closed`∉{0,1}/bad shape.
    Malformed,
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

pub(super) fn preflight_read(conn: &Connection) -> rusqlite::Result<Preflight> {
    if !table_exists(conn, "closure_state")? {
        return Ok(Preflight::LatchAbsent);
    }
    match conn.query_row("SELECT closed FROM closure_state WHERE id=1", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(0) => Ok(Preflight::LatchOpen),
        Ok(1) => Ok(Preflight::Sealed),
        // Table present but no id=1 latch row yet — unsealed, treat as
        // open so a fixture-created schema stays writable.
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Preflight::LatchOpen),
        Ok(_) | Err(_) => Ok(Preflight::Malformed),
    }
}

/// Deployment mode — chosen only inside authenticated daemon startup.
/// `Protected` is a *request*, not external authority.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenMode {
    /// Non-protected deployment — today's behavior minus protected
    /// downgrade paths.
    Legacy,
    /// Requested protected operation: forces every strict refusal; the
    /// production constructor is unreachable, so a real protected open
    /// yields `Unknown`.
    Protected,
}

/// What `Store::verified_rollback` proved about the conn after a
/// poisoned panic — distinguishes a conn that was *already clean*
/// (autocommit, no rollback needed) from one whose live tx was actually
/// rolled back, and both from an *unverified* state (rollback attempted
/// but the conn never returned to autocommit, so it may still be inside
/// a tx). Recovery writes and `rolled_back` telemetry must only claim a
/// rollback that actually happened.
pub(super) enum PoisonRecovery {
    /// The conn was already in autocommit — nothing was rolled back.
    CleanAutocommit,
    /// A live tx existed and the ROLLBACK verifiably returned the conn
    /// to autocommit.
    RolledBack,
    /// A tx was open but the ROLLBACK was denied/failed or the conn is
    /// still not autocommit — rollback is UNVERIFIED; record no
    /// `rolled_back` event and run no recovery write.
    Unverified,
}

/// Why a writable open was refused.
#[derive(Debug)]
pub enum SealError {
    /// Durable latch set — the file was sealed; refuse writable.
    Closed(String),
    /// Missing/uninitialized/malformed/non-WAL/foreign — never default-open.
    Unknown(String),
}
/// Read-only latch check for sibling writers that open `cadence.sqlite3`
/// outside the `Store` (rollout `connect`/`immediate`, backup
/// copy-transform sources). Refuses when the file carries a closure
/// latch — sealed *or* open-latch — or when it can't be read. A
/// latch-absent legacy db and a non-existent path pass, so scratch
/// copies and pre-init files are untouched.
pub(crate) fn preflight_writer_guard(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let ro = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| SealError::Unknown(format!("writer-guard open: {e}")))?;
    ro.busy_timeout(super::BUSY_TIMEOUT)
        .map_err(|e| SealError::Unknown(format!("writer-guard busy_timeout: {e}")))?;
    match preflight_read(&ro).map_err(|e| SealError::Unknown(format!("writer-guard read: {e}")))? {
        Preflight::LatchAbsent => Ok(()),
        Preflight::Sealed => {
            Err(SealError::Closed("producer closure latch is set — writes refused".into()).into())
        }
        Preflight::LatchOpen | Preflight::Malformed => Err(SealError::Unknown(
            "closure latch present on a sibling-writer path — refusing the write".into(),
        )
        .into()),
    }
}

impl From<SealError> for Error {
    fn from(e: SealError) -> Self {
        match e {
            SealError::Closed(why) => Error::rejected(format!("store sealed: {why}")),
            SealError::Unknown(why) => {
                Error::rejected(format!("store protected open unknown: {why}"))
            }
        }
    }
}

/// Recheck a legacy sibling writer after it has acquired BEGIN IMMEDIATE.
/// Only latch-absent databases qualify; this grants no protected authority.
/// Private callers must hold that transaction through their complete mutation.
pub(crate) fn require_legacy_writer_tx(tx: &Transaction<'_>) -> Result<()> {
    if tx.is_autocommit() {
        return Err(SealError::Unknown("sibling writer has no held transaction".into()).into());
    }
    match preflight_read(tx)
        .map_err(|e| SealError::Unknown(format!("held writer-guard read: {e}")))?
    {
        Preflight::LatchAbsent => Ok(()),
        Preflight::Sealed => {
            Err(SealError::Closed("producer closure latch is set — writes refused".into()).into())
        }
        Preflight::LatchOpen | Preflight::Malformed => Err(SealError::Unknown(
            "closure latch present on a sibling-writer path — refusing the write".into(),
        )
        .into()),
    }
}

/// What `preflight` decided for the open path.
#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub(super) enum PreflightDecision {
    /// Legacy, file absent or table-less — today's compatible open.
    LegacyFresh,
    /// Legacy, present non-WAL db — autocommit `journal_mode=WAL` allowed.
    LegacyConvertWal,
    /// Legacy, present WAL db — compatible open.
    LegacyOpen,
}

impl Store {
    /// Read-only preflight on the file — zero business writes before the
    /// decision. Runs *before* the writable `Connection::open`/WAL flip.
    pub(super) fn preflight(path: &Path, mode: OpenMode) -> Result<PreflightDecision> {
        if !path.exists() {
            return match mode {
                OpenMode::Protected => Err(SealError::Unknown(
                    "protected open requires an existing initialized database".into(),
                )
                .into()),
                OpenMode::Legacy => Ok(PreflightDecision::LegacyFresh),
            };
        }
        let ro = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| SealError::Unknown(format!("read-only open failed: {e}")))?;
        ro.busy_timeout(super::BUSY_TIMEOUT)
            .map_err(|e| SealError::Unknown(format!("busy_timeout: {e}")))?;
        let jmode: String = ro
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(|e| SealError::Unknown(format!("journal_mode read: {e}")))?;
        let has_schema = table_exists(&ro, "schema_version")
            .map_err(|e| SealError::Unknown(format!("schema_version read: {e}")))?;
        let latch =
            preflight_read(&ro).map_err(|e| SealError::Unknown(format!("latch read: {e}")))?;
        drop(ro);
        match mode {
            OpenMode::Protected => match latch {
                Preflight::Malformed => {
                    Err(SealError::Unknown("malformed protected latch".into()).into())
                }
                Preflight::Sealed => Err(SealError::Closed("database is sealed".into()).into()),
                Preflight::LatchAbsent => Err(SealError::Unknown(
                    "protected database carries no closure latch".into(),
                )
                .into()),
                Preflight::LatchOpen if jmode != "wal" => {
                    Err(SealError::Unknown("protected database is not WAL".into()).into())
                }
                // closed=0 exists, but no external init/restore factory — a
                // real production protected open is unreachable here.
                Preflight::LatchOpen => Err(SealError::Unknown(
                    "production protected open has no proven provenance — \
                     only synthetic test fixtures reach a guarded protected store"
                        .into(),
                )
                .into()),
            },
            OpenMode::Legacy => match latch {
                Preflight::Malformed => {
                    Err(SealError::Unknown("malformed closure latch".into()).into())
                }
                Preflight::Sealed | Preflight::LatchOpen => Err(SealError::Closed(
                    "database carries a protected closure latch".into(),
                )
                .into()),
                Preflight::LatchAbsent if !has_schema => Ok(PreflightDecision::LegacyFresh),
                Preflight::LatchAbsent if jmode != "wal" => Ok(PreflightDecision::LegacyConvertWal),
                Preflight::LatchAbsent => Ok(PreflightDecision::LegacyOpen),
            },
        }
    }

    /// The durable closure check, evaluated inside the held
    /// `BEGIN IMMEDIATE` so it serializes with the writer.
    fn check_closed_tx(tx: &Connection) -> Result<()> {
        match preflight_read(tx) {
            Ok(Preflight::LatchAbsent) | Ok(Preflight::LatchOpen) => Ok(()),
            Ok(Preflight::Sealed) => Err(SealError::Closed(
                "producer closure latch is set — writes refused".into(),
            )
            .into()),
            Err(e) => Err(SealError::Unknown(format!(
                "cannot read the closure latch — refusing the write: {e}"
            ))
            .into()),
            Ok(other) => Err(SealError::Unknown(format!(
                "cannot read the closure latch — refusing the write: {other:?}"
            ))
            .into()),
        }
    }

    /// The ONLY producer write entry point. Takes the conn mutex, opens
    /// `BEGIN IMMEDIATE`, re-checks the durable closure latch *inside*
    /// that tx, then hands the callback a restricted [`WriteTxn`] —
    /// closure-check and every DML are one SQLite writer critical
    /// section. A sealed store refuses before `f` runs; a callback error
    /// or panic rolls back inside the guard's private `TxControl` window
    /// (a bare `Transaction` drop would be denied by the authorizer).
    #[cfg(test)]
    pub(crate) fn with_sealed_tx<R>(
        &self,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> Result<R>,
    ) -> Result<R>
    where
        R: 'static,
    {
        self.sealed_tx(GuardState::BUSINESS, false, f)
    }

    /// [`Self::with_sealed_tx`] plus a fence re-check inside the held
    /// conn mutex — a writer fenced while waiting on the lock is refused
    /// before the arm and before `BEGIN`, so the fence/latch/DML decision
    /// is one serialized critical section. `fence` yields the refusal
    /// reason when the lease is lost; `|| None` passes always-armed.
    pub(crate) fn with_sealed_tx_fenced<R>(
        &self,
        fence: impl FnOnce() -> Option<String>,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> Result<R>,
    ) -> Result<R>
    where
        R: 'static,
    {
        self.sealed_tx_fenced(GuardState::BUSINESS, false, Some(fence), f)
    }

    /// Raw-error variant of [`Self::with_sealed_tx_fenced`].
    pub(crate) fn with_sealed_tx_fenced_raw<T>(
        &self,
        fence: impl FnOnce() -> Option<String>,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T>
    where
        T: 'static,
    {
        self.sealed_tx_fenced_raw(GuardState::BUSINESS, false, Some(fence), f)
    }

    fn sealed_tx<R>(
        &self,
        arm: u8,
        on_sealed: bool,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> Result<R>,
    ) -> Result<R>
    where
        R: 'static,
    {
        self.sealed_tx_fenced(arm, on_sealed, None::<fn() -> Option<String>>, f)
    }

    fn sealed_tx_fenced<R>(
        &self,
        arm: u8,
        on_sealed: bool,
        fence: Option<impl FnOnce() -> Option<String>>,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> Result<R>,
    ) -> Result<R>
    where
        R: 'static,
    {
        let guard = self.conn();
        let state = &*self.seal_state;
        // Re-check the hosted-lease fence INSIDE the held mutex — a
        // writer fenced while waiting on the lock is refused before the
        // arm and before `BEGIN`, so fence/latch/DML serialize together.
        if let Some(fence) = fence {
            if let Some(reason) = fence() {
                return Err(Error::rejected(format!(
                    "store write refused — the daemon's hosted lease is lost: {reason}"
                )));
            }
        }
        let _armed = ArmGuard::enter(state, arm);
        // `begin` runs in the guard's TxControl window so the authorizer
        // permits the tx boundary.
        let tx = {
            let _ctrl = ControlPhase::enter(state);
            Transaction::new_unchecked(&guard, TransactionBehavior::Immediate)
                .map_err(|e| Error::internal(e.to_string()))?
        };
        // Producer/business writes refuse a sealed latch. `witness_commit`
        // is the owner barrier that legitimately writes *after* seal — it
        // passes `on_sealed` and verifies the latch itself inside `f`.
        if !on_sealed {
            if let Err(e) = Self::check_closed_tx(&tx) {
                // A rollback failure is reported alongside the latch
                // refusal — never silently swallowed.
                Self::rollback_tx(state, &guard, tx)?;
                return Err(e);
            }
        }
        // (fall through) `f` runs with phase=Callback — the facade can
        // issue DML/read but no tx boundary; a panic is caught so the
        // rollback runs under TxControl, not a denied Transaction drop.
        // The facade borrow is scoped to the closure call so `tx` is
        // free to move/rollback/commit once it ends.
        let outcome = {
            let mut facade = WriteTxn { tx: &tx };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut facade)))
        };
        match outcome {
            Ok(Ok(v)) => {
                let _ctrl = ControlPhase::enter(state);
                tx.commit().map_err(|e| Error::internal(e.to_string()))?;
                Ok(v)
            }
            Ok(Err(e)) => {
                // Propagate a rollback failure over the callback error —
                // a still-open tx is a worse fault than the abort itself.
                Self::rollback_tx(state, &guard, tx)?;
                Err(e)
            }
            Err(payload) => {
                // On unwind we still must not leave the conn inside a
                // tx; log a rollback failure, then resume the panic.
                if let Err(rb) = Self::rollback_tx(state, &guard, tx) {
                    eprintln!("store: sealed-tx panic rollback failed: {rb}");
                }
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// `sealed_tx` with the rusqlite error channel preserved end-to-end —
    /// the callback and the boundary calls return `rusqlite::Result` so a
    /// caller classifies BUSY/constraint at the source.

    fn sealed_tx_fenced_raw<T>(
        &self,
        arm: u8,
        on_sealed: bool,
        fence: Option<impl FnOnce() -> Option<String>>,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T>
    where
        T: 'static,
    {
        let guard = self.conn();
        let state = &*self.seal_state;
        if let Some(fence) = fence {
            if let Some(reason) = fence() {
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    Error::rejected(format!(
                        "store write refused — the daemon's hosted lease is lost: {reason}"
                    )),
                )));
            }
        }
        let _armed = ArmGuard::enter(state, arm);
        let tx = {
            let _ctrl = ControlPhase::enter(state);
            Transaction::new_unchecked(&guard, TransactionBehavior::Immediate)?
        };
        if !on_sealed {
            if let Err(e) = Self::check_closed_tx(&tx) {
                if let Err(rb) = Self::rollback_tx(state, &guard, tx) {
                    return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(rb)));
                }
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(e)));
            }
        }
        let outcome = {
            let mut facade = WriteTxn { tx: &tx };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut facade)))
        };
        match outcome {
            Ok(Ok(v)) => {
                let _ctrl = ControlPhase::enter(state);
                tx.commit()?;
                Ok(v)
            }
            Ok(Err(e)) => {
                if let Err(rb) = Self::rollback_tx(state, &guard, tx) {
                    return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(rb)));
                }
                Err(e)
            }
            Err(payload) => {
                if let Err(rb) = Self::rollback_tx(state, &guard, tx) {
                    eprintln!("store: sealed-tx panic rollback failed: {rb}");
                }
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Roll the live `BEGIN IMMEDIATE` back inside the guard's private
    /// `TxControl` window — the authorizer denies a `ROLLBACK` issued at
    /// `phase=Callback`, so the owner performs it here, never the
    /// callback's own `Transaction` drop (which would be refused and
    /// leave the connection inside a tx).
    ///
    /// Verified, not assumed: `tx.rollback()` runs `ROLLBACK` under
    /// TxControl, then we check the *actual* `conn.is_autocommit()` — the
    /// connection is passed in explicitly precisely so the verification
    /// reads live state, not the `rollback()` return value. `Ok` only
    /// when the conn is genuinely back in autocommit; a `rollback()`
    /// error, or a conn still inside a tx afterwards, is a reported
    /// failure — never a silent `Ok`.
    fn rollback_tx(state: &GuardState, conn: &Connection, tx: Transaction<'_>) -> Result<()> {
        let rb_err = {
            let _ctrl = ControlPhase::enter(state);
            tx.rollback().err()
        };
        if let Some(e) = rb_err {
            return Err(Error::internal(format!("sealed-tx ROLLBACK failed: {e}")));
        }
        if !conn.is_autocommit() {
            return Err(Error::internal(
                "sealed-tx rollback did not return the connection to autocommit".to_string(),
            ));
        }
        Ok(())
    }

    /// Verified rollback for the *guard's* connection after a panic while
    /// the conn mutex was held — used by `conn()` poison recovery.
    /// Distinguishes clean-autocommit from an actual rollback, and both
    /// from `Unverified`: a `ROLLBACK` the disarmed authorizer refuses,
    /// or one that errors, or one after which the conn is still inside a
    /// tx, is **not** ignored — it returns `Unverified` so the caller
    /// never mislabels a still-open tx as `rolled_back`.
    pub(super) fn verified_rollback(state: &GuardState, conn: &Connection) -> PoisonRecovery {
        if conn.is_autocommit() {
            return PoisonRecovery::CleanAutocommit; // nothing to roll back
        }
        let rb = {
            let _ctrl = ControlPhase::enter(state);
            conn.execute_batch("ROLLBACK")
        };
        // Verify against live state, not the return value alone: a
        // denied or failed ROLLBACK — or one that left the conn inside a
        // tx — is unverified, never `RolledBack`.
        if rb.is_ok() && conn.is_autocommit() {
            PoisonRecovery::RolledBack
        } else {
            PoisonRecovery::Unverified
        }
    }

    /// Owner-maintenance write scope: takes the conn mutex, opens a held
    /// `BEGIN IMMEDIATE`, re-checks the durable latch *inside* that tx,
    /// then runs `f(&Connection)` armed `Owner`+`TxControl` — the latch
    /// check and every DDL/migration/DML commit atomically, so a second
    /// connection cannot seal between the check and the write. `f` sees
    /// the live tx's `&Connection` (a `Transaction` derefs to it) — never
    /// a business facade. Private: only the finite owner-maintenance ops
    /// below call it; the authorizer still denies the latch tables unless
    /// `Owner`. A sealed store refuses inside the tx before `f` runs.
    fn with_owner_tx<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let guard = self.conn();
        let state = &*self.seal_state;
        let _armed = ArmGuard::enter(state, GuardState::OWNER);
        // BEGIN IMMEDIATE in the tx-control window; the authorizer
        // permits the boundary only here.
        let tx = {
            let _ctrl = ControlPhase::enter(state);
            Transaction::new_unchecked(&guard, TransactionBehavior::Immediate)
                .map_err(|e| Error::internal(e.to_string()))?
        };
        // Latch check INSIDE the held tx — serializes the check+write
        // against any other writer on the file.
        if let Err(e) = Self::check_closed_tx(&tx) {
            Self::rollback_tx(state, &guard, tx)?;
            return Err(e);
        }
        // Catch a panic in `f` — a `Transaction` dropped on unwind would
        // run its auto-rollback *without* TxControl armed (the authorizer
        // denies it) and leave the conn inside a tx. Catch, then the
        // verified `rollback_tx` runs under the guard's own TxControl
        // window before resuming the unwind. Mirrors `sealed_tx`.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&tx)));
        match outcome {
            Ok(Ok(v)) => {
                let _ctrl = ControlPhase::enter(state);
                tx.commit().map_err(|e| Error::internal(e.to_string()))?;
                Ok(v)
            }
            Ok(Err(e)) => {
                Self::rollback_tx(state, &guard, tx)?;
                Err(e)
            }
            Err(payload) => {
                if let Err(rb) = Self::rollback_tx(state, &guard, tx) {
                    eprintln!("store: owner-tx panic rollback failed: {rb}");
                }
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Owner maintenance: record the running daemon build in the rollout
    /// guard table (DDL + upsert). Finite operation — no caller callback.
    pub(crate) fn owner_record_running_build(&self, commit: &str) -> Result<()> {
        self.with_owner_tx(|conn| {
            crate::rollout::upsert_daemon_build(conn, commit, crate::rollout::unix_now())
        })
    }

    /// Owner maintenance: enforce that this binary's commit is the one
    /// the daemon last recorded unless the caller holds the lease.
    pub(crate) fn owner_enforce_running_build(&self) -> Result<()> {
        self.with_owner_tx(crate::rollout::enforce_running_build)
    }

    /// Owner maintenance: fold a refused migration's side log into the
    /// daemon event stream (the refusal cannot be written into the db it
    /// is refusing to modify).
    pub(crate) fn owner_ingest_rollout_gate(&self, state_dir: &Path) -> Result<()> {
        self.with_owner_tx(|conn| crate::rollout::ingest_gate_log(state_dir, conn))
            .map(|_| ())
    }

    /// Test/fixture write scope: seeds schema or fixture rows on the
    /// restricted [`WriteTxn`] facade under the owner arm — fixture
    /// DDL/DML are authorized, the latch is still honored (a sealed
    /// store refuses), and the callback never reaches a `Connection`/
    /// `Statement`/tx-boundary. Test/seam only — never a production
    /// writer.
    #[cfg(any(test, feature = "test-seam"))]
    #[allow(dead_code)]
    pub(crate) fn fixture_write<R>(
        &self,
        f: impl for<'t> FnOnce(&mut WriteTxn<'t>) -> Result<R>,
    ) -> Result<R>
    where
        R: 'static,
    {
        // Owner arm so fixture DDL (CREATE/ALTER) is authorized; the
        // restricted WriteTxn facade still forbids a tx boundary or a
        // latch-table write at phase=Callback.
        self.sealed_tx(GuardState::OWNER, false, f)
    }

    /// The conn-`lock()` poison recovery: write the `store_poisoned`
    /// forensic row in ONE held `BEGIN IMMEDIATE` whose critical section
    /// covers the closure-latch re-check and the insert — never a
    /// process-local preflight read then an unrestricted owner write.
    /// Only an explicit safe unsealed outcome (`LatchAbsent`/`LatchOpen`)
    /// may write; a `Sealed`, `Malformed` or unreadable latch records
    /// nothing (a sealed or Protected-unknown store never produces a
    /// false `rolled_back`/`store_poisoned` row). `conn` is the held
    /// guard — this never re-locks `self.conn()`. `pub(super)` — the
    /// private `GuardState`/`&Connection` it takes is never a pub(crate)
    /// authority escape.
    pub(super) fn forensic_poison_event(
        state: &GuardState,
        conn: &Connection,
        rolled_back: bool,
        state_label: &'static str,
    ) {
        let _armed = ArmGuard::enter(state, GuardState::OWNER);
        let tx = {
            let _ctrl = ControlPhase::enter(state);
            match Transaction::new_unchecked(conn, TransactionBehavior::Immediate) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("store: forensic tx begin failed: {e}");
                    return;
                }
            }
        };
        // Latch check INSIDE the held tx — serializes with the DML.
        let safe = matches!(
            preflight_read(&tx),
            Ok(Preflight::LatchAbsent) | Ok(Preflight::LatchOpen)
        );
        if !safe {
            let _ = tx;
            return;
        }
        let out = Store::event(
            &tx,
            Store::DAEMON_STREAM,
            "store_poisoned",
            serde_json::json!({"rolled_back": rolled_back, "state": state_label}),
        );
        match out {
            Ok(()) => {
                let _ctrl = ControlPhase::enter(state);
                if let Err(e) = tx.commit() {
                    eprintln!("store: forensic commit failed: {e}");
                }
            }
            Err(e) => {
                eprintln!("store: could not record store_poisoned: {e}");
                let _ctrl = ControlPhase::enter(state);
                let _ = tx.rollback();
            }
        }
    }

    /// `propose_close` — flip the durable latch (owner lane), authorized
    /// by an [`OwnerMaintenancePermit`]. The permit is consumed one-use
    /// and its `challenge`/`attempt`/`epoch` are bound into the latch row
    /// so a later `witness_commit` must present the *same* permit binding.
    /// A caller can never mint the owner arm or a permit — synthetic in
    /// this draft (production `Protected` open is unreachable).
    #[allow(dead_code)]
    pub(super) fn propose_close(
        &self,
        permit: &OwnerMaintenancePermit,
        reason: &str,
    ) -> Result<()> {
        // A Close permit only — a Witness permit cannot propose the latch.
        permit.take(OwnerOp::Close)?;
        // The permit must be bound to THIS store's recorded local
        // identity — a permit minted for another database refuses.
        if permit.database_id() != self.db_identity {
            return Err(Error::rejected(
                "owner-maintenance permit is bound to a different database",
            ));
        }
        // Reject an epoch that cannot fit the latch's INTEGER column —
        // `as i64` would silently wrap a u64 > i64::MAX into a negative.
        let epoch = i64::try_from(permit.epoch)
            .map_err(|_| Error::rejected("owner-maintenance permit epoch overflows the latch"))?;
        self.sealed_tx(GuardState::OWNER, false, |wtx| {
            // Re-check the deadline + one-use inside the held tx — the
            // DML below must not outlive the permit's authority window.
            if permit.deadline_unix <= super::now() as i64 {
                return Err(Error::rejected(
                    "owner-maintenance permit is past its deadline",
                ));
            }
            wtx.execute_batch(SEAL_SCHEMA)?;
            // The latch binds challenge + attempt + the exact artifact +
            // epoch — a later witness must present the same binding.
            wtx.execute(
                "INSERT INTO closure_state(id,closed,reason,challenge,attempt,artifact,epoch,witness_done,closed_at)
                 VALUES(1,1,?,?,?,?,?,0,?)
                 ON CONFLICT(id) DO UPDATE SET closed=1,reason=excluded.reason,closed_at=excluded.closed_at
                   WHERE closure_state.witness_done=0",
                rusqlite::params![
                    reason,
                    permit.challenge.as_slice(),
                    permit.attempt.as_str(),
                    permit.artifact.as_str(),
                    epoch,
                    super::now()
                ],
            )?;
            Ok(())
        })
    }

    /// `witness_commit` — the single owner-maintenance write allowed on a
    /// sealed store: one `owner_witness` row + `witness_done=1` in one tx,
    /// authorized by an [`OwnerMaintenancePermit`]. The permit is consumed
    /// one-use and *every* binding — database identity, challenge, close
    /// attempt, artifact identity, epoch and deadline — must equal the
    /// durable latch sealed by `propose_close`; a mismatch refuses before
    /// a single row writes. Synthetic/test in this draft (production
    /// `Protected` open is unreachable).
    #[allow(dead_code)]
    pub(super) fn witness_commit(&self, permit: &OwnerMaintenancePermit) -> Result<u64> {
        // A Witness permit only — a Close permit cannot commit a witness.
        permit.take(OwnerOp::Witness)?;
        // The permit must be bound to THIS store's recorded local
        // identity.
        if permit.database_id() != self.db_identity {
            return Err(Error::rejected(
                "owner-maintenance permit is bound to a different database",
            ));
        }
        let epoch = i64::try_from(permit.epoch)
            .map_err(|_| Error::rejected("owner-maintenance permit epoch overflows the latch"))?;
        // The witness is the owner barrier that commits *after* the latch
        // is sealed — it runs `on_sealed` and verifies the latch in `f`.
        self.sealed_tx(GuardState::OWNER, true, |wtx| {
            // Deadline re-check inside the held tx before any DML.
            if permit.deadline_unix <= super::now() as i64 {
                return Err(Error::rejected(
                    "owner-maintenance permit is past its deadline",
                ));
            }
            let (closed, wdone, schallenge, sattempt, sartifact, sepoch): (
                i64,
                i64,
                Vec<u8>,
                String,
                Option<String>,
                i64,
            ) = wtx
                .query_row(
                    "SELECT closed,witness_done,challenge,attempt,artifact,epoch FROM closure_state WHERE id=1",
                    [],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .map_err(|_| Error::rejected("no sealed latch for the witness"))?;
            if closed != 1 {
                return Err(Error::rejected("store is not sealed"));
            }
            if wdone != 0 {
                return Err(Error::rejected("owner witness already committed"));
            }
            // Every permit binding must equal the sealed latch —
            // challenge, close attempt, the exact artifact AND the epoch.
            // A witness for a different binding than the one that sealed
            // refuses.
            if schallenge != permit.challenge
                || sattempt != permit.attempt
                || sartifact.as_deref() != Some(permit.artifact.as_str())
                || sepoch != epoch
            {
                return Err(Error::rejected(
                    "witness permit does not match the sealed latch",
                ));
            }
            wtx.execute(
                "INSERT INTO owner_witness(challenge,attempt,artifact_identity,epoch,db_schema,committed_at)
                 VALUES(?,?,?,?,?,?)",
                rusqlite::params![
                    permit.challenge.as_slice(),
                    permit.attempt.as_str(),
                    permit.artifact.as_str(),
                    epoch,
                    PROTECTED_SCHEMA_MAX,
                    super::now()
                ],
            )?;
            let seq = wtx.query_row("SELECT last_insert_rowid()", [], |r| r.get::<_, i64>(0))?;
            wtx.execute("UPDATE closure_state SET witness_done=1 WHERE id=1", [])?;
            Ok(seq as u64)
        })
    }
}

/// The shared read+DML surface a `Store` helper may use — implemented by
/// the read/owner `Connection` (and `Transaction`, which `Deref`s to it)
/// and by the business [`WriteTxn`] facade, so a `*.._in(conn, …)` helper
/// runs on a read guard, inside an open `Transaction`, or inside a sealed
/// business callback through one signature. Crucially it exposes **no
/// tx-boundary and no `&Connection` accessor** — the business facade
/// cannot reach a `Connection`/`Transaction`/`commit`/`rollback`, only
/// the read/DML verbs below. The authorizer still denies a boundary or
/// DDL the SQL text sneaks in at `phase=Callback`.
/// The restricted connection surface every read-guard and write-facade
/// shares: the `Connection`/`Transaction` DML+query verbs under their
/// rusqlite names, plus collecting helpers (`query_vec`)
/// that keep the `Statement` borrow local so no `Statement` escapes a
/// callback. Methods are generic over `rusqlite::Params`, so `params!`,
/// `[]`, `[x]` and `params_from_iter` all work unchanged — the trait is
/// used as `&impl StoreConn`, never `&dyn`.
///
/// `WriteTxn` additionally provides `execute`/`execute_batch`/`query_row`
/// inherent wrappers that flatten errors for prose callers; `StoreConn`
/// is how a business `*_in` helper written once runs on a read guard, a
/// live `Transaction`, and the sealed write facade alike.
pub(crate) trait StoreConn {
    fn execute(&self, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<usize>;
    fn execute_batch(&self, sql: &str) -> rusqlite::Result<()>;
    fn query_row<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T>;
    /// `query_row` + `optional()` folded in — `Ok(None)` on no rows.
    fn query_opt<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Option<T>>;
    /// `query_map` collected to a `Vec` — the `Statement` borrow stays
    /// inside the impl so none escapes a callback.
    fn query_vec<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Vec<T>>;
    fn last_insert_rowid(&self) -> i64;
    fn changes(&self) -> u64;
}

/// Implement `StoreConn` for a type that derefs to a `&Connection`
/// (`Connection`, `MutexGuard`, owner fixtures) — or a `&Transaction`
/// (`Transaction`, `WriteTxn` via its `tx`). `|$g| expr` yields the
/// `&Connection`/`&Transaction` to call the matching inherent verb on.
macro_rules! storeconn_impl {
    ($t:ty, |$g:ident| -> $ret:ty { $get:expr }) => {
        impl StoreConn for $t {
            fn execute(&self, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<usize> {
                let $g = self;
                ($get).execute(sql, params)
            }
            fn execute_batch(&self, sql: &str) -> rusqlite::Result<()> {
                let $g = self;
                ($get).execute_batch(sql)
            }
            fn query_row<T>(
                &self,
                sql: &str,
                params: impl rusqlite::Params,
                f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
            ) -> rusqlite::Result<T> {
                let $g = self;
                ($get).query_row(sql, params, f)
            }
            fn query_opt<T>(
                &self,
                sql: &str,
                params: impl rusqlite::Params,
                f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
            ) -> rusqlite::Result<Option<T>> {
                let $g = self;
                ($get).query_row(sql, params, f).optional()
            }
            fn query_vec<T>(
                &self,
                sql: &str,
                params: impl rusqlite::Params,
                f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
            ) -> rusqlite::Result<Vec<T>> {
                let $g = self;
                ($get).prepare(sql)?.query_map(params, f)?.collect()
            }
            fn last_insert_rowid(&self) -> i64 {
                let $g = self;
                ($get).last_insert_rowid()
            }
            fn changes(&self) -> u64 {
                let $g = self;
                ($get).changes()
            }
        }
    };
}

// Every impl resolves the getter to a `&Connection` and calls
// `Connection`'s inherent verbs — never the trait's same-named methods,
// which would recurse. `Transaction`/`WriteTxn` deref to `Connection`.
storeconn_impl!(Connection, |g| -> &Connection { g });
storeconn_impl!(Transaction<'_>, |g| -> &Connection { &**g });
storeconn_impl!(std::sync::MutexGuard<'_, Connection>, |g| -> &Connection {
    &**g
});
storeconn_impl!(WriteTxn<'_>, |g| -> &Connection { &**g.tx });

// Concrete `&`/`&mut`/guard/WriteTxn reference impls — each resolves to
// a `&Connection` so `Connection`'s inherent verbs run once (no trait
// recursion, no inherent-return-type mismatch). `conn`/`tx`/`&conn`/`&tx`
// at a `*_in(&impl StoreConn)` call site match one of these shapes.
storeconn_impl!(&Connection, |g| -> &Connection { *g });
storeconn_impl!(&mut Connection, |g| -> &Connection { &**g });
storeconn_impl!(&&Connection, |g| -> &Connection { **g });
storeconn_impl!(&&mut Connection, |g| -> &Connection { &***g });
storeconn_impl!(&std::sync::MutexGuard<'_, Connection>, |g| -> &Connection {
    &***g
});
storeconn_impl!(
    &mut std::sync::MutexGuard<'_, Connection>,
    |g| -> &Connection { &***g }
);
storeconn_impl!(
    &&std::sync::MutexGuard<'_, Connection>,
    |g| -> &Connection { &****g }
);
storeconn_impl!(
    &&mut std::sync::MutexGuard<'_, Connection>,
    |g| -> &Connection { &****g }
);
storeconn_impl!(&Transaction<'_>, |g| -> &Connection { &***g });
storeconn_impl!(&mut Transaction<'_>, |g| -> &Connection { &***g });
storeconn_impl!(&&Transaction<'_>, |g| -> &Connection { &****g });
storeconn_impl!(&&mut Transaction<'_>, |g| -> &Connection { &****g });
storeconn_impl!(&WriteTxn<'_>, |g| -> &Connection { &**g.tx });
storeconn_impl!(&mut WriteTxn<'_>, |g| -> &Connection { &**g.tx });
storeconn_impl!(&&WriteTxn<'_>, |g| -> &Connection { &**g.tx });
storeconn_impl!(&&mut WriteTxn<'_>, |g| -> &Connection { &**g.tx });

/// The restricted write facade handed to a business callback. Wraps the
/// live `Transaction`; exposes only DML/read
/// — never `commit`/`rollback`/tx-boundary, and never a `Connection` or
/// `Transaction` borrow that outlives the closure. `rusqlite`-typed
/// methods preserve the raw error so callers classify a BUSY/constraint
/// at the source; the crate-`Error` variants flatten for prose callers.
pub struct WriteTxn<'t> {
    tx: &'t Transaction<'t>,
}

impl<'t> WriteTxn<'t> {
    /// Raw-error `execute` — preserves `rusqlite::Error` (BUSY/constraint
    /// classification survives; the crate `Error` would flatten it).
    pub(crate) fn execute_raw(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<usize> {
        self.tx.execute(sql, params)
    }
    /// `execute` — `pub` so external `tests/` hooks can drive DML on the
    /// restricted facade; the facade itself still forbids tx-boundary/
    /// `commit`/`rollback`/`Connection` escape at `phase=Callback`.
    pub fn execute(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
        self.execute_raw(sql, params).map_err(Into::into)
    }
    /// DML batch on the live tx — `pub` for test hooks; the authorizer
    /// still denies any tx-boundary/DDL the batch tries to sneak in (a
    /// `COMMIT`/`BEGIN` inside `sql` is denied at `phase=Callback`).
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.tx.execute_batch(sql).map_err(Into::into)
    }
    /// `query_row` — `pub` for test hooks that probe the tx.
    pub fn query_row<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<T> {
        self.query_row_raw(sql, params, f).map_err(Into::into)
    }
    /// Raw-error `query_row` — keeps `QueryReturnedNoRows`/BUSY intact.
    pub(crate) fn query_row_raw<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        self.tx.query_row(sql, params, f)
    }
    /// `query_map` returning the collected Vec — `pub` for test hooks;
    /// the `Statement` borrow stays inside the impl so none escapes.
    /// Flattened to the crate `Result` like the other facade verbs.
    pub fn query_map<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        let mut stmt = self.tx.prepare(sql).map_err(Error::from)?;
        let rows = stmt.query_map(params, f).map_err(Error::from)?;
        rows.collect::<rusqlite::Result<Vec<T>>>()
            .map_err(Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// A synthetic protected-store path: a `Legacy` open (fixture) that
    /// then installs the seal schema. This is the *only* way tests reach a
    /// latched db — production `Protected` construction stays `Unknown`.
    fn open_legacy(dir: &TempDir) -> (std::path::PathBuf, Store) {
        let db = dir.path().join("t.sqlite3");
        (db.clone(), Store::open(&db).unwrap())
    }

    /// A synthetic owner-maintenance permit bound to `db` — the only way
    /// tests authorize `propose_close`/`witness_commit`. Far-future
    /// deadline so expiry is not under test here.
    fn permit(
        db: &std::path::Path,
        op: OwnerOp,
        challenge: &[u8],
        attempt: &str,
        artifact: &str,
        epoch: u64,
    ) -> OwnerMaintenancePermit {
        // Bind to the same canonicalized-path identity `Store::open`
        // recorded, so the permit proves it was issued for THIS db.
        OwnerMaintenancePermit::synthetic(
            &db.canonicalize()
                .unwrap_or_else(|_| db.to_path_buf())
                .to_string_lossy(),
            op,
            challenge,
            attempt,
            artifact,
            epoch,
            crate::store::now() as i64 + 3_600,
        )
    }

    #[test]
    fn unsealed_business_write_then_sealed_refuses_every_writer() {
        let dir = TempDir::new().unwrap();
        let (db, s) = open_legacy(&dir);
        // A producer write goes through.
        s.event_public("daemon", "probe", json!({})).unwrap();
        // Seal it via the owner lane.
        s.propose_close(
            &permit(&db, OwnerOp::Close, b"chal", "attempt-1", "", 7),
            "test",
        )
        .unwrap();
        // Every producer write now refuses.
        assert!(s.event_public("daemon", "probe", json!({})).is_err());
        assert!(s
            .with_sealed_tx(|wtx| Ok(wtx.execute_batch("SELECT 1")?))
            .is_err());
        // Reopen: the durable latch survives — a fresh Store on the file
        // refuses writable open.
        drop(s);
        assert!(Store::open(&db).is_err(), "sealed file must refuse reopen");
    }

    #[test]
    fn business_writer_cannot_touch_latch_or_witness_tables() {
        let dir = TempDir::new().unwrap();
        let (_db, s) = open_legacy(&dir);
        // Create the latch tables through the owner lane (unsealed db).
        s.propose_close(&permit(&_db, OwnerOp::Close, b"c", "a", "", 1), "t")
            .unwrap();
        // Reopen is refused on the sealed file, so exercise the *armed*
        // business conn on a fresh unsealed db that also carries the
        // latch: a Business-armed conn must not write closure_state or
        // owner_witness even though it can write business tables.
        let dir2 = TempDir::new().unwrap();
        let (_db2, s2) = open_legacy(&dir2);
        s2.propose_close(&permit(&_db2, OwnerOp::Close, b"c", "a", "", 1), "t")
            .unwrap();
        // s2's conn is the armed path — but sealing already closed it.
        // Use a third, unsealed store whose latch tables exist via a
        // *second* owner pass is impossible; instead assert the authorizer
        // semantics directly: a business conn that tries latch writes is
        // denied at PREPARE. Build an unsealed store, inject latch tables
        // via the owner-armed `fixture_write` lane (restricted WriteTxn),
        // then drop to a fresh business conn.
        let dir3 = TempDir::new().unwrap();
        let (_db3, s3) = open_legacy(&dir3);
        // Inject the latch tables through the owner-armed fixture write
        // lane (restricted WriteTxn) — DDL is authorized, no raw conn.
        s3.fixture_write(|wtx| Ok(wtx.execute_batch(SEAL_SCHEMA)?))
            .unwrap();
        // A business WriteTxn must not write latch/witness tables even
        // though it can write business tables.
        s3.with_sealed_tx(|wtx| {
            for sql in [
                "INSERT INTO closure_state(id,closed) VALUES(1,0)",
                "UPDATE closure_state SET closed=0",
                "DELETE FROM closure_state",
                "INSERT INTO owner_witness(challenge,attempt,artifact_identity,epoch) VALUES(x'00','a','x',1)",
                "UPDATE owner_witness SET attempt='x'",
                "DELETE FROM owner_witness",
            ] {
                assert!(
                    wtx.execute_batch(sql).is_err(),
                    "business lane must not write latch/witness: {sql}"
                );
            }
            // Business tables still writable on the same armed facade.
            wtx.execute_batch(
                "INSERT INTO messages(id,alias,body,state) VALUES('m','a','b','q')",
            )
            .ok(); // table may not exist in this fixture; the point is the latch deny
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn protected_mode_open_refuses_missing_and_legacy_and_sealed() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("nope.sqlite3");
        // Missing file under Protected -> Unknown.
        assert!(Store::open_mode(&missing, OpenMode::Protected).is_err());
        // A legacy (latch-absent) db under Protected -> Unknown (no latch).
        let (_db, s) = open_legacy(&dir);
        drop(s);
        assert!(Store::open_mode(&_db, OpenMode::Protected).is_err());
        // A sealed db refuses under both modes.
        let dir2 = TempDir::new().unwrap();
        let (db2, s2) = open_legacy(&dir2);
        s2.propose_close(&permit(&db2, OwnerOp::Close, b"c", "a", "", 1), "t")
            .unwrap();
        drop(s2);
        assert!(Store::open_mode(&db2, OpenMode::Protected).is_err());
        assert!(Store::open_mode(&db2, OpenMode::Legacy).is_err());
    }

    #[test]
    fn legacy_open_of_latch_present_db_refuses() {
        let dir = TempDir::new().unwrap();
        let (db, s) = open_legacy(&dir);
        s.propose_close(&permit(&db, OwnerOp::Close, b"c", "a", "", 1), "t")
            .unwrap();
        drop(s);
        // Legacy open of a latch-carrying db refuses (no autocommit
        // conversion or recovery on a protected file).
        assert!(Store::open(&db).is_err());
    }

    #[test]
    fn open_side_refuses_under_protected() {
        let dir = TempDir::new().unwrap();
        let (db, _s) = open_legacy(&dir);
        assert!(Store::open_side_mode(&db, OpenMode::Protected).is_err());
        // Legacy open_side still allowed for an unsealed legacy db.
        assert!(Store::open_side_mode(&db, OpenMode::Legacy).is_ok());
    }

    #[test]
    fn witness_commit_requires_seal_and_replays_refuse() {
        let dir = TempDir::new().unwrap();
        let (_db, s) = open_legacy(&dir);
        // Not sealed -> witness refuses.
        assert!(s
            .witness_commit(&permit(&_db, OwnerOp::Witness, b"c", "a", "art", 1))
            .is_err());
        s.propose_close(
            &permit(&_db, OwnerOp::Close, b"ch", "attempt-x", "art", 9),
            "t",
        )
        .unwrap();
        // Wrong challenge -> refuse.
        assert!(s
            .witness_commit(&permit(
                &_db,
                OwnerOp::Witness,
                b"WRONG",
                "attempt-x",
                "art",
                9
            ))
            .is_err());
        // Correct -> one row, seq 1.
        let seq = s
            .witness_commit(&permit(
                &_db,
                OwnerOp::Witness,
                b"ch",
                "attempt-x",
                "art",
                9,
            ))
            .unwrap();
        assert_eq!(seq, 1);
        // Replay -> witness_done refuses.
        assert!(s
            .witness_commit(&permit(
                &_db,
                OwnerOp::Witness,
                b"ch",
                "attempt-x",
                "art",
                9
            ))
            .is_err());
    }

    /// Adversarial: a business-armed write conn must not mutate connection
    /// state, checkpoint, or schema via pragma. Each of these is denied by
    /// the authorizer while `phase=Callback`/`armed=Business`.
    #[test]
    fn business_writer_cannot_pragma_mutate_or_checkpoint() {
        let dir = TempDir::new().unwrap();
        let (_db, s) = open_legacy(&dir);
        s.with_sealed_tx(|wtx| {
            for sql in [
                "PRAGMA optimize",
                "PRAGMA optimize=0x10002",
                "PRAGMA journal_mode=WAL",
                "PRAGMA wal_checkpoint(TRUNCATE)",
                "PRAGMA wal_checkpoint",
                "PRAGMA busy_timeout=1000",
                "PRAGMA synchronous=OFF",
                "PRAGMA foreign_keys=ON",
                "PRAGMA user_version=9",
            ] {
                assert!(
                    wtx.execute_batch(sql).is_err(),
                    "business lane must refuse mutating/conn-state pragma: {sql}"
                );
            }
            // Read-form pragmas still pass in the business lane.
            wtx.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                .map_err(|e| Error::internal(format!("journal_mode read denied: {e}")))?;
            for sql in ["PRAGMA user_version", "PRAGMA page_count"] {
                wtx.query_row(sql, [], |r| r.get::<_, i64>(0))
                    .map_err(|e| Error::internal(format!("read pragma {sql} denied: {e}")))?;
            }
            Ok(())
        })
        .unwrap();
    }

    /// Adversarial: `load_extension` and the extension/fs/tokenizer surface
    /// are denied in every lane — including the owner lane.
    #[test]
    fn extension_and_dangerous_functions_denied() {
        let dir = TempDir::new().unwrap();
        let (_db, s) = open_legacy(&dir);
        s.with_sealed_tx(|wtx| {
            for sql in [
                "SELECT load_extension('/tmp/x')",
                "SELECT fts3_tokenizer('x')",
                "SELECT readfile('/etc/passwd')",
                "SELECT writefile('/tmp/x','y')",
                "SELECT sqlite_source_id()",
            ] {
                assert!(
                    wtx.execute_batch(sql).is_err(),
                    "dangerous/extension function must be denied: {sql}"
                );
            }
            Ok(())
        })
        .unwrap();
        // The exact DENIED list is what the Function arm refuses; assert
        // each named function is not in the allowlist so a future edit
        // can't silently re-admit one.
        for f in DENIED_FUNCTIONS {
            assert!(!ALLOWED_FUNCTIONS.contains(f), "{f} must stay denied");
        }
    }

    /// Adversarial: a business `WriteTxn` callback runs at
    /// `phase=Callback` — BEGIN/COMMIT/SAVEPOINT/ROLLBACK it sneaks in are
    /// denied by the authorizer even though the facade is armed. The
    /// facade itself exposes no boundary method; this is the SQL-text
    /// escape hatch that must stay shut.
    #[test]
    fn callback_phase_cannot_begin_commit_or_savepoint() {
        let dir = TempDir::new().unwrap();
        let (_db, s) = open_legacy(&dir);
        s.with_sealed_tx(|wtx| {
            for sql in [
                "BEGIN",
                "BEGIN IMMEDIATE",
                "COMMIT",
                "SAVEPOINT x",
                "RELEASE x",
                "ROLLBACK",
                "END",
            ] {
                assert!(
                    wtx.execute_batch(sql).is_err(),
                    "callback phase must refuse tx boundary: {sql}"
                );
            }
            Ok(())
        })
        .unwrap();
    }

    /// Unknown/`Attach`/`Detach`/`Analyze`/`Reindex`/`Vtable` and any future
    /// `AuthAction` are deny-default. Probe the ones constructible via SQL.
    #[test]
    fn disarmed_and_business_deny_schema_and_attach_escape() {
        let dir = TempDir::new().unwrap();
        let (_db, s) = open_legacy(&dir);
        s.with_sealed_tx(|wtx| {
            for sql in [
                "ATTACH DATABASE '/tmp/e.db' AS e",
                "CREATE TABLE x(id INTEGER)",
                "CREATE INDEX i ON messages(id)",
                "CREATE TEMP VIEW v AS SELECT 1",
                "CREATE TRIGGER t AFTER INSERT ON messages BEGIN SELECT 1; END",
                "ANALYZE",
                "REINDEX",
                "CREATE VIRTUAL TABLE v USING fts5(a)",
                "ALTER TABLE messages ADD COLUMN x TEXT",
                "DROP TABLE messages",
            ] {
                assert!(
                    wtx.execute_batch(sql).is_err(),
                    "business lane must refuse schema/attach escape: {sql}"
                );
            }
            Ok(())
        })
        .unwrap();
    }

    /// Regression: the allowlist covers the functions the store's real
    /// business SQL calls — a function the production code needs must not
    /// be denied. Exercises a broad DML+function surface through an armed
    /// business write.
    #[test]
    fn business_lane_allows_needed_functions() {
        let dir = TempDir::new().unwrap();
        let (_db, s) = open_legacy(&dir);
        s.with_sealed_tx(|wtx| {
            for sql in [
                "SELECT coalesce(NULL,1)",
                "SELECT length(randomblob(4))",
                "SELECT json_extract('{\"a\":1}','$.a')",
                "SELECT count(*) FROM sqlite_schema",
                "SELECT strftime('%Y','now') IS NOT NULL",
            ] {
                wtx.query_row(sql, [], |r| r.get::<_, i64>(0))
                    .map_err(|e| Error::internal(format!("needed function denied: {sql}: {e}")))?;
            }
            Ok(())
        })
        .unwrap();
    }

    /// Adversarial: a panic inside a `with_owner_tx` callback must not
    /// leave the conn inside a tx — the verified rollback runs under the
    /// guard's own TxControl window before the unwind resumes. Assert the
    /// conn is back in autocommit and a later write still works.
    #[test]
    fn owner_tx_panic_rolls_back_and_returns_to_autocommit() {
        let dir = TempDir::new().unwrap();
        let (_db, s) = open_legacy(&dir);
        // Drive a REAL owner-maintenance tx (`with_owner_tx`) that runs a
        // DML then panics — catch_unwind inside the guard rolls the tx
        // back under TxControl, then the unwind resumes and drops the
        // conn mutex guard (poisoning it). `conn()`'s recovery then sees
        // a conn already in autocommit — the rollback happened BEFORE
        // any poison-forensic masking could hide a leaked tx.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = s.with_owner_tx(|conn| {
                conn.execute(
                    "INSERT INTO events(alias,kind,payload,at) VALUES('owner-probe','panic','{}',0)",
                    [],
                )?;
                assert_eq!(conn.query_row(
                    "SELECT count(*) FROM events WHERE alias='owner-probe'",
                    [], |row| row.get::<_, i64>(0),
                )?, 1, "the owner DML must actually run before panic");
                panic!("deliberate panic after owner DML");
            });
        }));
        assert!(r.is_err(), "panic must propagate");
        // The DML must NOT have committed — the rollback discarded it.
        // Inspect the poisoned mutex directly. Store::conn() repairs a
        // leaked transaction and would hide a broken owner rollback.
        let held = s
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            held.is_autocommit(),
            "owner rollback must finish before recovery"
        );
        let present: i64 = held
            .query_row(
                "SELECT COUNT(*) FROM events WHERE alias='owner-probe'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(present, 0, "owner-tx DML must roll back on panic");
        drop(held);
        // A later sanctioned business write still works — the conn is
        // usable, not wedged inside a leaked tx.
        s.event_public("daemon", "after-panic", json!({}))
            .unwrap_or_else(|e| panic!("store unusable after panic rollback: {e}"));
    }

    /// Adversarial: the owner-maintenance permit is bound to the database
    /// it was issued for, the operation it authorizes, and every latch
    /// field (challenge, attempt, artifact, epoch) — plus deadline and
    /// one-use. Each mismatch must refuse; only the fully-matching permit
    /// commits.
    #[test]
    fn owner_permit_rejects_every_binding_mismatch() {
        let dir = TempDir::new().unwrap();
        let (db, s) = open_legacy(&dir);
        let id = db
            .canonicalize()
            .unwrap_or_else(|_| db.clone())
            .to_string_lossy()
            .into_owned();
        let dl = crate::store::now() as i64 + 3_600;

        // Wrong database identity -> refuse.
        let wrong_db = OwnerMaintenancePermit::synthetic(
            "/nonexistent/other.sqlite3",
            OwnerOp::Close,
            b"ch",
            "a",
            "art",
            1,
            dl,
        );
        assert!(s.propose_close(&wrong_db, "t").is_err());

        // Correct Close permit seals; replaying it refuses (one-use).
        let close =
            OwnerMaintenancePermit::synthetic(&id, OwnerOp::Close, b"ch", "a", "art", 7, dl);
        s.propose_close(&close, "t").unwrap();
        // A fresh Close permit on the now-sealed store also refuses the
        // write (witness_done blocks it inside the tx) — and anyway the
        // latch is already closed.
        let close2 =
            OwnerMaintenancePermit::synthetic(&id, OwnerOp::Close, b"ch", "a", "art", 7, dl);
        assert!(s.propose_close(&close2, "t").is_err());

        // Wrong operation: a Close permit cannot commit a witness.
        let as_close =
            OwnerMaintenancePermit::synthetic(&id, OwnerOp::Close, b"ch", "a", "art", 7, dl);
        assert!(s.witness_commit(&as_close).is_err());

        // Each field mismatch on a Witness permit refuses.
        for (ch, at, art, ep, label) in [
            (&b"ch"[..], "a", "art", 1u64, "wrong epoch"),
            (b"WRONG", "a", "art", 7, "wrong challenge"),
            (b"ch", "wrong-attempt", "art", 7, "wrong attempt"),
            (b"ch", "a", "WRONG-artifact", 7, "wrong artifact"),
        ] {
            let p = OwnerMaintenancePermit::synthetic(&id, OwnerOp::Witness, ch, at, art, ep, dl);
            assert!(s.witness_commit(&p).is_err(), "witness must refuse {label}");
        }
        // Expired deadline refuses.
        let stale = OwnerMaintenancePermit::synthetic(
            &id,
            OwnerOp::Witness,
            b"ch",
            "a",
            "art",
            7,
            crate::store::now() as i64 - 1,
        );
        assert!(s.witness_commit(&stale).is_err());
        // Epoch that overflows i64 refuses the cast.
        let overflow = OwnerMaintenancePermit::synthetic(
            &id,
            OwnerOp::Witness,
            b"ch",
            "a",
            "art",
            u64::MAX,
            dl,
        );
        assert!(s.witness_commit(&overflow).is_err());
        // A correct Witness permit commits once; a second use refuses.
        let good =
            OwnerMaintenancePermit::synthetic(&id, OwnerOp::Witness, b"ch", "a", "art", 7, dl);
        let seq = s.witness_commit(&good).unwrap();
        assert_eq!(seq, 1);
        // Fresh permit, same binding -> replay refused (witness_done=1).
        let replay =
            OwnerMaintenancePermit::synthetic(&id, OwnerOp::Witness, b"ch", "a", "art", 7, dl);
        assert!(s.witness_commit(&replay).is_err());
    }

    /// Both handles exist before the latch. The second SQLite connection's
    /// real busy handler proves it reached the contended BEGIN IMMEDIATE;
    /// bounded channels release the first commit before its retry. No sleeps
    /// or reopening a sealed file stand in for an actual competing writer.
    #[test]
    fn second_connection_seal_serializes_with_business_write() {
        use std::sync::{mpsc, Mutex};
        use std::time::Duration;
        struct BusyGate {
            entered: mpsc::SyncSender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        static GATE: Mutex<Option<Arc<BusyGate>>> = Mutex::new(None);
        fn busy(count: i32) -> bool {
            if count != 0 {
                return false;
            }
            let Some(gate) = GATE.lock().unwrap().clone() else {
                return false;
            };
            if gate.entered.try_send(()).is_err() {
                return false;
            }
            let released = gate
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .is_ok();
            released
        }
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                *GATE.lock().unwrap() = None;
            }
        }
        let _reset = Reset;
        for close_first in [false, true] {
            let dir = TempDir::new().unwrap();
            let (db, first) = open_legacy(&dir);
            let second = Store::open(&db).unwrap();
            let (held_tx, held_rx) = mpsc::sync_channel(1);
            let (commit_tx, commit_rx) = mpsc::sync_channel(1);
            let (busy_tx, busy_rx) = mpsc::sync_channel(1);
            let (retry_tx, retry_rx) = mpsc::sync_channel(1);
            *GATE.lock().unwrap() = Some(Arc::new(BusyGate {
                entered: busy_tx,
                release: Mutex::new(retry_rx),
            }));
            second.conn().busy_handler(Some(busy)).unwrap();
            let callback_ran = std::sync::atomic::AtomicBool::new(false);
            std::thread::scope(|scope| {
                let first_ref = &first;
                let holding = scope.spawn(move || {
                    if close_first {
                        // Test-only private owner setup holds the actual close
                        // DML until commit. It proves lock/latch ordering, not
                        // external authority or a production permit factory.
                        first_ref.with_owner_tx(|conn| {
                            conn.execute_batch(SEAL_SCHEMA)?;
                            conn.execute("INSERT INTO closure_state(id,closed,reason,challenge,attempt,artifact,epoch,witness_done,closed_at) VALUES(1,1,'test',X'63','a','art',1,0,0)", [])?;
                            held_tx.send(()).unwrap();
                            commit_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                            Ok(())
                        })
                    } else {
                        first_ref.with_sealed_tx(|wtx| {
                            wtx.execute("INSERT INTO events(alias,kind,payload,at) VALUES('race-probe','probe','{}',0)", [])?;
                            held_tx.send(()).unwrap();
                            commit_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                            Ok(())
                        })
                    }
                });
                held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                let pending = scope.spawn(|| {
                    if close_first {
                        second.with_sealed_tx(|wtx| {
                            callback_ran.store(true, Ordering::SeqCst);
                            wtx.execute("INSERT INTO events(alias,kind,payload,at) VALUES('race-late','probe','{}',0)", [])?;
                            Ok(())
                        })
                    } else {
                        second.propose_close(&permit(&db, OwnerOp::Close, b"c", "a", "art", 1), "test")
                    }
                });
                busy_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                // The second BEGIN is really blocked by the first connection.
                assert!(!callback_ran.load(Ordering::SeqCst));
                commit_tx.send(()).unwrap();
                holding.join().unwrap().unwrap();
                retry_tx.send(()).unwrap();
                let result = pending.join().unwrap();
                if close_first {
                    assert!(result.is_err());
                } else {
                    result.unwrap();
                }
            });
            assert!(!callback_ran.load(Ordering::SeqCst));
            let callback = std::sync::atomic::AtomicBool::new(false);
            assert!(first
                .with_sealed_tx(|_| {
                    callback.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .is_err());
            assert!(
                !callback.load(Ordering::SeqCst),
                "late writer must refuse before its callback"
            );
            let rows: i64 = second
                .conn()
                .query_row(
                    "SELECT count(*) FROM events WHERE alias IN ('race-probe','race-late')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(rows, if close_first { 0 } else { 1 });
            second.conn().busy_handler(None).unwrap();
            *GATE.lock().unwrap() = None;
        }
    }
}
