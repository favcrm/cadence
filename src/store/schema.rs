//! Open, schema migrations, restart recovery and adoption.

use crate::adapter::registry;
use crate::error::{Error, Result};
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use super::effects;
use super::messages::{Message, FENCING_UNKNOWN_SQL};
use super::platform;
use super::threads;
use super::{Store, BUSY_TIMEOUT};

/// Read-only open of the daemon store from another process, with the
/// shared busy timeout — never creates or migrates the file.
pub(crate) fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

/// Result of [`Store::take_queued`].
pub enum Take {
    /// Agent is disabled; the actor should exit.
    Stop,
    /// Nothing queued.
    Empty,
    /// Claimed for submission.
    Message(Box<Message>),
}

/// A pty turn recorded by a provably clean daemon shutdown (CAD-89).
/// `recover()` protects the message while the store-level checks pass;
/// the actor re-validates the pane (`pane_pid` still owns
/// `native_session`) before the turn is truly re-adopted.
#[derive(Debug, Clone)]
pub struct AdoptEntry {
    pub alias: String,
    pub message_id: String,
    pub turn_id: String,
    /// Endpoint generation the turn's token embeds — preserved across
    /// the restart so a later report still validates.
    pub generation: String,
    pub pane_pid: u32,
    pub native_session: String,
}

/// What `serve()` consumed from the state-dir shutdown marker before
/// opening the store: the recorded entries plus a staleness reason when
/// the marker itself failed validation (wrong instance, expired) —
/// entries in a stale marker are refused one by one in `recover()`.
/// `failed` carries the error a DRAIN failure recorded instead: the
/// previous run's `shutdown_entries` never committed, so every
/// in-flight row it left is unproven and `recover()` must fence each
/// one with the refusal event its verdict reads.
///
/// The two Option fields are independent axes (CAD-694): `stale` is
/// freshness — the pane-adoption TTL — while `failed` is provenance,
/// valid whenever `instance` names the immediately preceding recorded
/// run even after the TTL has lapsed. `stale.is_some()` and
/// `failed.is_some()` CAN hold together (an expired marker from a
/// predecessor whose drain still failed); `failed` on a marker whose
/// `instance` does not match is never populated — `serve()` drops it
/// before this is built.
pub struct ConsumedMarker {
    /// The daemon-instance id the consumed marker claimed — "" when the
    /// marker carried none.
    pub instance: String,
    pub entries: Vec<AdoptEntry>,
    pub stale: Option<String>,
    /// The drain error the marker recorded; `None` on a clean stop.
    /// Set only for a marker that names the immediately preceding
    /// recorded run — unlike `entries` this is provenance, not
    /// freshness, so TTL expiry does not clear it.
    pub failed: Option<String>,
}

/// What `recover()` fenced and what marker informed it — returned by
/// `open_adopting` so the daemon can persist it as this start's
/// recovery record (CAD-694). A restart verdict reads the record to
/// learn whether the predecessor's drain and this sweep are provable —
/// evidence the per-alias event cursors cannot always carry (a
/// predecessor already dead leaves no cursor to compare against, and
/// non-pty endpoints were never cursor-covered).
#[derive(Debug, Default)]
pub struct RecoveryOutcome {
    /// Instance id the consumed marker claimed; `None` when no marker
    /// was consumed.
    pub marker_instance: Option<String>,
    /// Staleness provenance recorded at consume (wrong instance or
    /// expired); `None` on a valid marker or no marker.
    pub stale: Option<String>,
    /// The drain failure the predecessor recorded, if any.
    pub failed: Option<String>,
    /// Every in-flight row this recovery fenced (swept to `unknown`,
    /// never adopted): (alias, message_id).
    pub fenced: Vec<(String, String)>,
    /// `fenced` rows carrying no refusal event anywhere — a crash or a
    /// drain that lost its writes left them unproven.
    pub unevidenced: Vec<(String, String)>,
}

impl Store {
    /// Open (creating if needed), migrate, and recover in-flight state.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_mode(path, super::seal::OpenMode::Legacy)
    }

    /// `open` with an explicit deployment mode. `Protected` is a startup
    /// *request* (agent_uid/hosted.lease provenance) that forces strict
    /// refusals — never external restore/init authority.
    pub fn open_mode(path: &Path, mode: super::seal::OpenMode) -> Result<Self> {
        Self::open_adopting_mode(path, None, mode).map(|(store, _)| store)
    }

    /// `open` with the consumed hot-restart marker (CAD-89): `serve()`
    /// reads and validates `shutdown.json` before this — the store only
    /// sees the candidate entries and a staleness reason. Every other
    /// caller passes `None` and gets the historical fence-everything
    /// recovery. The `RecoveryOutcome` reports what that recovery
    /// fenced so the daemon can persist it for the restart verdict
    /// (CAD-694).
    pub fn open_adopting(
        path: &Path,
        marker: Option<ConsumedMarker>,
    ) -> Result<(Self, RecoveryOutcome)> {
        Self::open_adopting_mode(path, marker, super::seal::OpenMode::Legacy)
    }

    /// `open_adopting` with an explicit deployment mode (CAD-1011).
    pub fn open_adopting_mode(
        path: &Path,
        marker: Option<ConsumedMarker>,
        mode: super::seal::OpenMode,
    ) -> Result<(Self, RecoveryOutcome)> {
        let (store, outcome) = Self::open_inner(path, marker, true, true, mode)?;
        Ok((store, outcome.expect("open_adopting always recovers")))
    }

    /// Open a live database for a side write that must not run restart
    /// recovery. `app remove` uses this to revoke derived grants while
    /// the daemon still holds the store — recovery would fence the
    /// daemon's in-flight turns (CAD-577).
    pub fn open_side(path: &Path) -> Result<Self> {
        Self::open_side_mode(path, super::seal::OpenMode::Legacy)
    }

    /// `open_side` with a mode — under `Protected` a second writer
    /// connection to `cadence.sqlite3` refuses outright (CAD-1011).
    pub fn open_side_mode(path: &Path, mode: super::seal::OpenMode) -> Result<Self> {
        if mode == super::seal::OpenMode::Protected {
            return Err(Error::rejected(
                "open_side refused: a second writer connection to a protected \
                 cadence.sqlite3 is not permitted",
            ));
        }
        Self::open_inner(path, None, true, false, mode).map(|(store, _)| store)
    }

    /// Migrate an older database without the rollout lease gate.
    ///
    /// Schema-migration tests use this to replay a downgraded file.
    /// `open` and `open_adopting` — the daemon and doctor paths — never
    /// call it, so a lower-schema production database still refuses.
    pub fn open_for_schema_tests(path: &Path) -> Result<Self> {
        Self::open_inner(path, None, false, true, super::seal::OpenMode::Legacy)
            .map(|(store, _)| store)
    }

    fn open_inner(
        path: &Path,
        marker: Option<ConsumedMarker>,
        gate: bool,
        recover: bool,
        mode: super::seal::OpenMode,
    ) -> Result<(Self, Option<RecoveryOutcome>)> {
        let permit = if gate {
            crate::rollout::authorize_migration(path)?
        } else {
            crate::rollout::MigrationPermit { crossing: None }
        };
        // CAD-1011: durable-closure preflight BEFORE any write or WAL
        // mutation — read-only, so a sealed/protected file is never
        // touched. Decides whether legacy WAL conversion may run.
        let decision = Self::preflight(path, mode)?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // Fail-closed authorizer: disarmed until a guarded tx arms it.
        let seal_state = std::sync::Arc::new(super::seal::GuardState::default());
        super::seal::install_authorizer(&conn, seal_state.clone());
        // Only a legacy pre-latch db may take the autocommit WAL
        // conversion; a protected db is already WAL by provision and a
        // latch-carrying file can never reach this conversion.
        if matches!(decision, super::seal::PreflightDecision::LegacyFresh)
            || matches!(decision, super::seal::PreflightDecision::LegacyConvertWal)
        {
            // The autocommit WAL conversion is owner-maintenance — armed so
            // the authorizer lets the value-bearing `journal_mode=WAL`
            // pragma through (the business lane can never write it).
            super::seal::with_owner_tx_control(&seal_state, || -> Result<()> {
                conn.pragma_update(None, "journal_mode", "WAL")
                    .map_err(Error::from)
            })?;
        }
        // CAD-1011: the schema bootstrap + every migration run under the
        // constructor's owner-maintenance + tx-control window — the only
        // scope the authorizer allows DDL/migration writes in.
        super::seal::with_owner_tx_control(&seal_state, || -> Result<()> {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version(version INTEGER NOT NULL);
             INSERT INTO schema_version(version)
               SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM schema_version);",
            )?;
            let version: i64 =
                conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
            if version < 1 {
                conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS agents(
                    alias TEXT PRIMARY KEY, provider TEXT NOT NULL,
                    endpoint_kind TEXT NOT NULL, role TEXT NOT NULL,
                    cwd TEXT NOT NULL, sandbox TEXT NOT NULL,
                    instructions TEXT, thread_id TEXT, session_id TEXT,
                    model TEXT, effort TEXT, pid INTEGER,
                    state TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
                    error TEXT, created REAL NOT NULL, updated REAL NOT NULL);
                 CREATE TABLE IF NOT EXISTS messages(
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    id TEXT UNIQUE NOT NULL, alias TEXT NOT NULL,
                    body TEXT NOT NULL, reply_to TEXT, source TEXT NOT NULL,
                    state TEXT NOT NULL DEFAULT 'queued',
                    turn_id TEXT, result TEXT, error TEXT,
                    created REAL NOT NULL, started REAL, completed REAL);
                 CREATE INDEX IF NOT EXISTS msg_queue ON messages(alias,state,seq);
                 CREATE TABLE IF NOT EXISTS events(
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    alias TEXT NOT NULL, kind TEXT NOT NULL,
                    payload TEXT NOT NULL, at REAL NOT NULL);
                 UPDATE schema_version SET version=1;",
                )?;
            }
            if version < 2 {
                // Atomic: the column add and version bump commit together, so
                // a crash cannot leave version=1 with the column present
                // (which would permanently fail the next ALTER). The column
                // check makes an already half-applied state converge instead
                // of erroring on a duplicate column.
                let has_endpoint = conn
                    .prepare("PRAGMA table_info(agents)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .any(|name| name == "endpoint");
                let tx = conn.unchecked_transaction()?;
                if !has_endpoint {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN endpoint TEXT")?;
                }
                tx.execute("UPDATE schema_version SET version=2", [])?;
                tx.commit()?;
            }
            if version < 3 {
                // v3: `params` holds endpoint-specific registration options
                // (pty: native session to resume); `generation` is the live
                // endpoint generation minted per `open` for stale-token
                // rejection. Same atomic column-check + transaction pattern
                // as v2.
                let columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(agents)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !columns.iter().any(|c| c == "params") {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN params TEXT")?;
                }
                if !columns.iter().any(|c| c == "generation") {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN generation TEXT")?;
                }
                tx.execute("UPDATE schema_version SET version=3", [])?;
                tx.commit()?;
            }
            if version < 4 {
                // v4: the work axis. `jobs`/`tasks`/`verdicts` tables plus
                // attachment columns on `messages` (`task_id`) and `events`
                // (`job_id`/`task_id`). One transaction, existence checks
                // before each ALTER, `IF NOT EXISTS` on the new objects —
                // a half-applied v4 converges on reopen like v2/v3. Old
                // messages simply read task_id NULL (unattached delivery).
                let msg_cols: Vec<String> = conn
                    .prepare("PRAGMA table_info(messages)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let event_cols: Vec<String> = conn
                    .prepare("PRAGMA table_info(events)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS jobs(
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    spec_path TEXT NOT NULL,
                    spec_sha256 TEXT,
                    pm_alias TEXT NOT NULL,
                    issue_id TEXT,
                    repo TEXT,
                    base_ref TEXT,
                    state TEXT NOT NULL,
                    max_revisions INTEGER NOT NULL DEFAULT 2,
                    error TEXT,
                    created REAL NOT NULL, updated REAL NOT NULL);
                 CREATE TABLE IF NOT EXISTS tasks(
                    id TEXT PRIMARY KEY,
                    job_id TEXT NOT NULL REFERENCES jobs(id),
                    title TEXT,
                    role TEXT NOT NULL DEFAULT 'implementer',
                    assignee TEXT,
                    spec_path TEXT,
                    acceptance TEXT,
                    worktree TEXT,
                    branch TEXT,
                    base_sha TEXT,
                    head_sha TEXT,
                    state TEXT NOT NULL,
                    revision INTEGER NOT NULL DEFAULT 0,
                    dispatch_message TEXT,
                    error TEXT,
                    created REAL NOT NULL, updated REAL NOT NULL);
                 CREATE INDEX IF NOT EXISTS tasks_job ON tasks(job_id, state);
                 CREATE TABLE IF NOT EXISTS verdicts(
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id TEXT NOT NULL REFERENCES tasks(id),
                    revision INTEGER NOT NULL,
                    sha TEXT NOT NULL,
                    verdict TEXT NOT NULL,
                    reviewer TEXT NOT NULL,
                    evidence TEXT,
                    message TEXT,
                    created REAL NOT NULL);
                 CREATE INDEX IF NOT EXISTS verdicts_task ON verdicts(task_id, revision);",
                )?;
                if !msg_cols.iter().any(|c| c == "task_id") {
                    tx.execute_batch("ALTER TABLE messages ADD COLUMN task_id TEXT")?;
                }
                if !event_cols.iter().any(|c| c == "job_id") {
                    tx.execute_batch("ALTER TABLE events ADD COLUMN job_id TEXT")?;
                }
                if !event_cols.iter().any(|c| c == "task_id") {
                    tx.execute_batch("ALTER TABLE events ADD COLUMN task_id TEXT")?;
                }
                tx.execute_batch(
                    "CREATE INDEX IF NOT EXISTS msg_task ON messages(task_id);
                 CREATE INDEX IF NOT EXISTS events_job ON events(job_id, seq);
                 UPDATE schema_version SET version=4;",
                )?;
                tx.commit()?;
            }
            if version < 5 {
                // v5: `verdicts.verify` — the CLI's worktree-verification
                // result ({checked, skipped}) stored with the verdict it
                // gated (CAD-51). Same atomic column-check + transaction
                // pattern as v2/v3; old rows read verify NULL.
                let columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(verdicts)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !columns.iter().any(|c| c == "verify") {
                    tx.execute_batch("ALTER TABLE verdicts ADD COLUMN verify TEXT")?;
                }
                tx.execute("UPDATE schema_version SET version=5", [])?;
                tx.commit()?;
            }
            if version < 6 {
                // v6: `jobs.stall_secs` — the per-job silence budget for
                // stall detection (CAD-52). NULL leaves resolution to the
                // assignee's `stall_secs` param, then the daemon default.
                let columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(jobs)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !columns.iter().any(|c| c == "stall_secs") {
                    tx.execute_batch("ALTER TABLE jobs ADD COLUMN stall_secs INTEGER")?;
                }
                tx.execute("UPDATE schema_version SET version=6", [])?;
                tx.commit()?;
            }
            if version < 8 {
                // v8: daemon-owned supervision registrations. PR #80 owns v7
                // for provider-confirmed effort. Preserve that v7 schema contract
                // when CAD-176 lands first: a later v7 migration will be skipped
                // at version 8, so the prerequisite column must already exist.
                // This bridge carries schema compatibility only; provider effort
                // reporting remains owned by v7. Coverage is a separate table so
                // the observer never expands a project name into implicit task
                // membership. Alert uniqueness binds one monitor to one observed
                // event fingerprint across restarts.
                let agent_columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(agents)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !agent_columns.iter().any(|column| column == "effort") {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN effort TEXT")?;
                }
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS monitors(
                    id TEXT PRIMARY KEY,
                    project TEXT NOT NULL,
                    owner TEXT NOT NULL,
                    interval_secs INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    heartbeat_at REAL,
                    last_check_at REAL,
                    last_success_at REAL,
                    next_check_at REAL,
                    event_cursor INTEGER NOT NULL DEFAULT 0,
                    delivery_configured INTEGER NOT NULL DEFAULT 0,
                    delivery_state TEXT NOT NULL DEFAULT 'unconfigured',
                    dispatch_enabled INTEGER NOT NULL DEFAULT 0,
                    error TEXT,
                    created REAL NOT NULL,
                    updated REAL NOT NULL);
                 CREATE TABLE IF NOT EXISTS monitor_tasks(
                    monitor_id TEXT NOT NULL,
                    task_id TEXT NOT NULL,
                    PRIMARY KEY(monitor_id, task_id));
                 CREATE INDEX IF NOT EXISTS monitor_tasks_task
                    ON monitor_tasks(task_id, monitor_id);
                 CREATE TABLE IF NOT EXISTS monitor_alerts(
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    monitor_id TEXT NOT NULL,
                    task_id TEXT NOT NULL,
                    event_seq INTEGER NOT NULL,
                    fingerprint TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    state TEXT NOT NULL DEFAULT 'open',
                    attempts INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    created REAL NOT NULL,
                    updated REAL NOT NULL,
                    UNIQUE(monitor_id, fingerprint));
                 CREATE INDEX IF NOT EXISTS monitor_alerts_monitor
                    ON monitor_alerts(monitor_id, seq);
                 UPDATE schema_version SET version=8;",
                )?;
                tx.commit()?;
            }
            if version < 9 {
                // v9: provider-owned allowance telemetry. It is kept in its own
                // column so caller-editable `params` can never become quota
                // evidence. The column check makes a half-applied migration
                // converge on reopen. CAD-114 owns this schema slot.
                let columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(agents)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !columns.iter().any(|column| column == "quota") {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN quota TEXT")?;
                }
                tx.execute("UPDATE schema_version SET version=9", [])?;
                tx.commit()?;
            }
            if version < 10 {
                // v10: background dispatch is a separate, durable consent from
                // the v8 manual `dispatch_enabled` bit. Keep the old bit's
                // meaning stable so an existing registration cannot begin
                // dispatching merely because the daemon was upgraded. This
                // migration also repairs a schema-9 database made by an older
                // PR100 candidate, which used v9 for this monitor column before
                // the provider quota owner claimed v9. Thus either PR can be
                // landed first without silently skipping the other column.
                let agent_columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(agents)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let monitor_columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(monitors)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !agent_columns.iter().any(|column| column == "quota") {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN quota TEXT")?;
                }
                if !monitor_columns
                    .iter()
                    .any(|column| column == "auto_dispatch_enabled")
                {
                    tx.execute_batch(
                        "ALTER TABLE monitors
                     ADD COLUMN auto_dispatch_enabled INTEGER NOT NULL DEFAULT 0",
                    )?;
                }
                tx.execute("UPDATE schema_version SET version=10", [])?;
                tx.commit()?;
            }
            if version < 11 {
                // v11: daemon-wide model defaults plus per-agent team role and
                // model provenance. Existing rows stay null so resume does not
                // re-resolve a default that did not exist when they launched.
                let columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(agents)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !columns.iter().any(|column| column == "team_role") {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN team_role TEXT")?;
                }
                if !columns.iter().any(|column| column == "model_selection") {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN model_selection TEXT")?;
                }
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS model_defaults(
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    revision INTEGER NOT NULL,
                    document TEXT NOT NULL);
                 INSERT INTO model_defaults(id, revision, document)
                   SELECT 1, 0, '{\"schema\":1,\"providers\":{}}'
                   WHERE NOT EXISTS (SELECT 1 FROM model_defaults WHERE id = 1);
                 UPDATE schema_version SET version=11;",
                )?;
                tx.commit()?;
            }
            if version < 12 {
                // v12: rollout lease + the build commit the daemon last
                // recorded. The lease gate runs before this function opens
                // the file; reaching here means the crossing was allowed
                // (fresh database, current schema, matching backup receipt,
                // or the one-time CADENCE_ROLLOUT_BOOTSTRAP introduction).
                let tx = conn.unchecked_transaction()?;
                crate::rollout::ensure_lease_tables(&tx)?;
                tx.execute("UPDATE schema_version SET version=12", [])?;
                tx.commit()?;
            }
            if version < 13 {
                // v13: durable conversation threads (CAD-319) — a thread per
                // agent alias and its ordered entries, outliving provider
                // sessions. New objects only, `IF NOT EXISTS`, one
                // transaction: a half-applied v13 converges on reopen.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(threads::SCHEMA_V13)?;
                tx.execute("UPDATE schema_version SET version=13", [])?;
                tx.commit()?;
            }
            if version < 14 {
                // v14: `agents.pid_start` (CAD-385) — the recorded pid's
                // process start time, so a reused pid never maps to a stale
                // row's alias. Existing rows stay NULL: they fail closed
                // until their endpoint is recorded again (recovery below
                // clears every live pid; adoption re-records it with its
                // start). Column add + bump in one transaction, and the add
                // is skipped when present: a half-applied v14 converges.
                let columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(agents)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !columns.iter().any(|column| column == "pid_start") {
                    tx.execute_batch("ALTER TABLE agents ADD COLUMN pid_start INTEGER")?;
                }
                tx.execute("UPDATE schema_version SET version=14", [])?;
                tx.commit()?;
            }
            if version < 15 {
                // v15: `messages.priority` (CAD-158) — the delivery rank
                // `take_queued` orders by (urgent first, then arrival).
                // Existing rows read 0 = normal, so an upgraded queue keeps
                // its FIFO order. Column add + bump in one transaction, the
                // add skipped when present: a half-applied v15 converges.
                let columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(messages)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                if !columns.iter().any(|column| column == "priority") {
                    tx.execute_batch(
                        "ALTER TABLE messages ADD COLUMN priority INTEGER NOT NULL DEFAULT 0",
                    )?;
                }
                tx.execute("UPDATE schema_version SET version=?1", [15])?;
                tx.commit()?;
            }
            if version < 16 {
                // v16: `messages.issue`/`messages.worktree` (CAD-467) —
                // the dispatch lane a message row belongs to, written at
                // send by the daemon. The reported-kickoff duplicate check
                // matches on these, never on the issue's refs — tracker
                // frontmatter is forgeable and a planted `message` ref must
                // not be able to suppress a real kickoff.
                let columns: Vec<String> = conn
                    .prepare("PRAGMA table_info(messages)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                let tx = conn.unchecked_transaction()?;
                for column in ["issue", "worktree"] {
                    if !columns.iter().any(|c| c == column) {
                        tx.execute_batch(&format!(
                            "ALTER TABLE messages ADD COLUMN {column} TEXT"
                        ))?;
                    }
                }
                tx.execute("UPDATE schema_version SET version=?1", [16])?;
                tx.commit()?;
            }
            if version < 17 {
                // v17: platform custody records, per-agent grants and
                // project default accounts (CAD-366, ADR 0006 §5.1/§5.3) —
                // handles and fingerprints only, never credential bytes.
                // New objects, `IF NOT EXISTS`, one transaction: a
                // half-applied v17 converges on reopen.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(platform::SCHEMA_V17)?;
                tx.execute("UPDATE schema_version SET version=?1", [17])?;
                tx.commit()?;
            }
            if version < 18 {
                // v18: the durable pending-effect record and the draft log
                // (CAD-506, ADR 0006 §5.4) — one row per staged send, keyed
                // by effect_id with the brokered handle UNIQUE, so a retried
                // open dedupes and a restart reconciles. Input/summary/
                // preview only — credentials never land here.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(effects::SCHEMA_V18)?;
                tx.execute("UPDATE schema_version SET version=?1", [18])?;
                tx.commit()?;
            }
            if version < 19 {
                // v19: app-derived grants and `install_id` (CAD-577) — the install a
                // derived grant belongs to. Empty on rows written before
                // install ids; those never match the current install, so
                // the sweep withdraws them and the operator re-approves
                // once. Released v18 has no app_grants table; intermediate
                // builds have one without install_id. Create/alter/version
                // bump share a transaction, preserving either upgrade path.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(platform::SCHEMA_APP_GRANTS)?;
                let columns: Vec<String> = tx
                    .prepare("PRAGMA table_info(app_grants)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                if !columns.iter().any(|column| column == "install_id") {
                    tx.execute_batch(
                        "ALTER TABLE app_grants ADD COLUMN install_id TEXT NOT NULL DEFAULT ''",
                    )?;
                }
                tx.execute("UPDATE schema_version SET version=?1", [19])?;
                tx.commit()?;
            }
            if version < 20 {
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(super::app_runs::SCHEMA)?;
                tx.execute("UPDATE schema_version SET version=20", [])?;
                tx.commit()?;
            }
            if version < 21 {
                let tx = conn.unchecked_transaction()?;
                let mut statement = tx.prepare("PRAGMA table_info(platform_credentials)")?;
                let columns = statement
                    .query_map([], |r| r.get::<_, String>(1))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                if !columns.iter().any(|c| c == "connection_id") {
                    tx.execute_batch("ALTER TABLE platform_credentials ADD COLUMN connection_id TEXT NOT NULL DEFAULT '';")?;
                }
                if !columns.iter().any(|c| c == "credential_revision") {
                    tx.execute_batch("ALTER TABLE platform_credentials ADD COLUMN credential_revision INTEGER NOT NULL DEFAULT 1;")?;
                }
                tx.execute_batch("UPDATE platform_credentials SET connection_id='conn-' || lower(hex(randomblob(16))) WHERE connection_id=''; CREATE UNIQUE INDEX IF NOT EXISTS platform_connection_id ON platform_credentials(connection_id); CREATE TABLE IF NOT EXISTS connection_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),workspace_id TEXT NOT NULL); INSERT OR IGNORE INTO connection_metadata VALUES(1,lower(hex(randomblob(16))));")?;
                tx.execute("UPDATE schema_version SET version=21", [])?;
                tx.commit()?;
            }
            if version < 22 {
                // Context rows and the nullable association are additive. Keep
                // historical snapshots, approvals and provider receipts untouched;
                // DDL and the schema checkpoint either commit together or roll back.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(super::app_contexts::SCHEMA)?;
                let columns = tx
                    .prepare("PRAGMA table_info(app_runs)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if !columns.iter().any(|column| column == "context_id") {
                    tx.execute_batch(
                    "ALTER TABLE app_runs ADD COLUMN context_id TEXT REFERENCES app_contexts(id);",
                )?;
                }
                tx.execute_batch("CREATE INDEX IF NOT EXISTS app_runs_context ON app_runs(install_id,context_id,created);")?;
                tx.execute("UPDATE schema_version SET version=22", [])?;
                tx.commit()?;
            }
            if version < 23 {
                // The actual predecessor is merged CAD690's schema22. Legacy
                // effects remain agent-grant records; an app child never falls
                // back to them. DDL, discriminator and checkpoint are atomic.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(super::app_bindings::SCHEMA)?;
                tx.execute_batch(super::app_effects::SCHEMA)?;
                let columns = tx
                    .prepare("PRAGMA table_info(platform_effects)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if !columns.iter().any(|name| name == "authorization_kind") {
                    tx.execute_batch("ALTER TABLE platform_effects ADD COLUMN authorization_kind TEXT NOT NULL DEFAULT 'agent_grant' CHECK(authorization_kind IN ('agent_grant','app_artifact'));")?;
                }
                tx.execute("UPDATE schema_version SET version=23", [])?;
                tx.commit()?;
            }
            if version < 24 {
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(super::app_capabilities::SCHEMA)?;
                tx.execute("UPDATE schema_version SET version=24", [])?;
                tx.commit()?;
            }
            if version < 25 {
                // A review can additionally pin one immutable, run-scoped binary
                // capability receipt. Both columns and the version advance commit
                // together; existing text-only reviews retain NULL asset fields.
                let tx = conn.unchecked_transaction()?;
                for (table, column, definition) in [
                    (
                        "app_run_reviews",
                        "asset_receipt_id",
                        "asset_receipt_id TEXT",
                    ),
                    ("app_run_reviews", "asset_digest", "asset_digest TEXT"),
                    (
                        "app_capability_results",
                        "receipt_schema",
                        "receipt_schema INTEGER NOT NULL DEFAULT 1",
                    ),
                ] {
                    let columns = tx
                        .prepare(&format!("PRAGMA table_info({table})"))?
                        .query_map([], |row| row.get::<_, String>(1))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    if !columns.iter().any(|name| name == column) {
                        tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {definition};"))?;
                    }
                }
                tx.execute("UPDATE schema_version SET version=25", [])?;
                tx.commit()?;
            }
            if version < 26 {
                // CAD-720: historical v26 source rows are inert. No migration
                // backfills older messages: their cloud enrollment and restore
                // generation are unknown. v27 fences every v26 row as local.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS cloud_dispatch_outbox(
                    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
                    message_id TEXT NOT NULL UNIQUE REFERENCES messages(id),
                    source TEXT NOT NULL CHECK(source IN ('dispatch','job_dispatch')),
                    task_id TEXT,
                    task_revision INTEGER,
                    audience_agent TEXT NOT NULL,
                    expected_head TEXT,
                    payload_digest TEXT NOT NULL,
                    organization_id TEXT,
                    remote_turn_id TEXT UNIQUE,
                    created REAL NOT NULL,
                    claimed REAL,
                    CHECK ((task_id IS NULL AND task_revision IS NULL)
                        OR (task_id IS NOT NULL AND task_revision IS NOT NULL)),
                    CHECK ((organization_id IS NULL AND remote_turn_id IS NULL AND claimed IS NULL)
                        OR (organization_id IS NOT NULL AND remote_turn_id IS NOT NULL AND claimed IS NOT NULL))
                );
                CREATE TRIGGER IF NOT EXISTS cloud_dispatch_source_immutable
                BEFORE UPDATE ON cloud_dispatch_outbox
                WHEN NEW.message_id IS NOT OLD.message_id
                  OR NEW.source IS NOT OLD.source
                  OR NEW.task_id IS NOT OLD.task_id
                  OR NEW.task_revision IS NOT OLD.task_revision
                  OR NEW.audience_agent IS NOT OLD.audience_agent
                  OR NEW.expected_head IS NOT OLD.expected_head
                  OR NEW.payload_digest IS NOT OLD.payload_digest
                  OR NEW.created IS NOT OLD.created
                  OR (OLD.remote_turn_id IS NOT NULL AND (
                      NEW.organization_id IS NOT OLD.organization_id
                      OR NEW.remote_turn_id IS NOT OLD.remote_turn_id
                      OR NEW.claimed IS NOT OLD.claimed))
                BEGIN SELECT RAISE(ABORT, 'cloud dispatch source or claim is immutable'); END;
                UPDATE schema_version SET version=26;",
            )?;
                tx.commit()?;
            }
            if version < 27 {
                // Every v26 row was created by a local dispatch without issuer
                // enrollment. Preserve it for audit, but make it permanently
                // ineligible for a future cloud turn claim. No current insert
                // path is permitted to create an eligible row either.
                let tx = conn.unchecked_transaction()?;
                let columns = tx
                    .prepare("PRAGMA table_info(cloud_dispatch_outbox)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if !columns.iter().any(|name| name == "cloud_eligible") {
                    tx.execute_batch(
                        "ALTER TABLE cloud_dispatch_outbox ADD COLUMN
                     cloud_eligible INTEGER NOT NULL DEFAULT 0 CHECK(cloud_eligible=0);",
                    )?;
                }
                let invalid: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM cloud_dispatch_outbox WHERE cloud_eligible IS NOT 0",
                    [],
                    |row| row.get(0),
                )?;
                if invalid != 0 {
                    return Err(crate::error::Error::rejected(
                        "cloud dispatch eligibility contains an unverified row",
                    ));
                }
                tx.execute_batch(
                    "DROP TRIGGER IF EXISTS cloud_dispatch_eligibility_immutable;
                 DROP TRIGGER IF EXISTS cloud_dispatch_eligibility_insert_guard;
                 CREATE TRIGGER cloud_dispatch_eligibility_immutable
                 BEFORE UPDATE ON cloud_dispatch_outbox
                 WHEN NEW.cloud_eligible IS NOT OLD.cloud_eligible
                 BEGIN SELECT RAISE(ABORT, 'cloud dispatch eligibility is immutable'); END;
                 CREATE TRIGGER cloud_dispatch_eligibility_insert_guard
                 BEFORE INSERT ON cloud_dispatch_outbox
                 WHEN NEW.cloud_eligible IS NOT 0
                 BEGIN SELECT RAISE(ABORT, 'cloud dispatch eligibility is unverified'); END;
                 UPDATE schema_version SET version=27;",
                )?;
                tx.commit()?;
            }
            if version < 28 {
                // A package upgrade preserves the exact approval epoch of completed
                // work. Backfill only the current, independently recorded epoch;
                // older epochs discarded by pre-v28 code cannot be reconstructed.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS app_capability_epochs(
                    install_id TEXT NOT NULL, epoch INTEGER NOT NULL CHECK(epoch>0),
                    digest TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('approved','revoked')),
                    created REAL NOT NULL, PRIMARY KEY(install_id,epoch));
                 INSERT OR IGNORE INTO app_capability_epochs(install_id,epoch,digest,state,created)
                 SELECT install_id,epoch,digest,state,created FROM app_install_capabilities;
                 DROP INDEX IF EXISTS app_binding_live_slot;
                 CREATE UNIQUE INDEX IF NOT EXISTS app_binding_live_slot_version
                 ON app_bindings(install_id,scope_key,slot,
                   coalesce(json_extract(config,'$.bundle_digest'),''))
                 WHERE state='configured';
                 UPDATE schema_version SET version=28;",
            )?;
                tx.commit()?;
            }
            if version < 29 {
                // CAD-771: durable scheduled external-post intents. New table
                // only; existing rows and code paths are untouched.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(super::social_publish::SCHEMA)?;
                tx.execute("UPDATE schema_version SET version=29", [])?;
                tx.commit()?;
            }
            if version < 30 {
                // CAD-785: one host-custodied SMTP sender link per CRM
                // installation/context. New table only; the secret itself
                // lives in custody, never in this row.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(super::crm_smtp::SCHEMA)?;
                tx.execute("UPDATE schema_version SET version=30", [])?;
                tx.commit()?;
            }
            if version < 31 {
                // CAD-786: the PII-free send intent (`crm_sends`, crash
                // reconciliation only) and the hash-only unsubscribe
                // index. Recipient rows never live in core.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(super::crm_sends::SCHEMA)?;
                tx.execute("UPDATE schema_version SET version=31", [])?;
                tx.commit()?;
            }
            if let Some(crossing) = permit.crossing {
                Self::event(
                    &conn,
                    Self::DAEMON_STREAM,
                    "rollout_migration_allowed",
                    json!({
                        "from": crossing.from,
                        "to": crate::rollout::SCHEMA_VERSION,
                        "reason": crossing.reason,
                    }),
                )?;
            }
            Ok(())
        })?;
        let store = Self {
            conn: Mutex::new(conn),
            write_fence: Default::default(),
            adoptions: Mutex::new(std::collections::HashMap::new()),
            thread_held: Mutex::new(std::collections::HashMap::new()),
            shutdown_entries_hook: None,
            shutdown_backoff_ms: 50,
            seal_state,
        };
        let outcome = if recover {
            Some(store.recover(marker.as_ref())?)
        } else {
            None
        };
        Ok((store, outcome))
    }

    /// A restart cannot know whether an in-flight provider turn executed.
    /// Mark those attempts `unknown` and fence the owning actor; do not
    /// silently relaunch it. Inbox rows are durable mailboxes, not
    /// processes — their pseudo-endpoint and `idle` state survive.
    ///
    /// The exception is the hot restart (CAD-89): a provably clean
    /// shutdown recorded each pty agent's `running` turn. An entry whose
    /// store-level checks still pass stays `running` — its `generation`
    /// is cleared like every other runtime field, so a report landing
    /// before the pane proof is refused as stale — and the actor
    /// re-validates the pane itself at open; `set_identity_adopted`
    /// then restores the recorded generation and the token validates
    /// again. Anything else falls back to the fence below, one
    /// `turn_adopt_refused` event per rejected entry.
    fn recover(&self, marker: Option<&ConsumedMarker>) -> Result<RecoveryOutcome> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        // Store-level qualification of every recorded entry. A refused
        // entry still lands in the sweep below — the refusal only means
        // "not protected", never a state skip.
        let mut kept: Vec<&AdoptEntry> = Vec::new();
        // Message ids refused in this transaction — the evidence check
        // below must not read them as unproven.
        let mut refused: Vec<String> = Vec::new();
        if let Some(marker) = marker {
            for e in &marker.entries {
                let reason = marker.stale.clone().or_else(|| self.adoption_block(&tx, e));
                match reason {
                    None => kept.push(e),
                    Some(reason) => {
                        Self::event(
                            &tx,
                            &e.alias,
                            "turn_adopt_refused",
                            json!({"message": e.message_id, "turn_id": e.turn_id,
                                   "reason": reason}),
                        )?;
                        refused.push(e.message_id.clone());
                    }
                }
            }
        }
        // One pane proof covers a whole alias list, so every entry in
        // it must describe the SAME endpoint facts — `shutdown_entries`
        // writes one tuple per alias, but a hand-built or corrupt
        // marker can carry divergent records. Refuse the list as a
        // unit: adopting `entries[0]`'s pane for a sibling recorded
        // elsewhere would be an unverified open.
        {
            let mut by_alias: std::collections::HashMap<&str, Vec<usize>> =
                std::collections::HashMap::new();
            for (i, e) in kept.iter().enumerate() {
                by_alias.entry(e.alias.as_str()).or_default().push(i);
            }
            let mut dropped: Vec<usize> = Vec::new();
            for idxs in by_alias.values() {
                let first = kept[idxs[0]];
                let divergent = idxs[1..].iter().any(|&i| {
                    kept[i].generation != first.generation
                        || kept[i].pane_pid != first.pane_pid
                        || kept[i].native_session != first.native_session
                });
                if divergent {
                    dropped.extend_from_slice(idxs);
                }
            }
            if !dropped.is_empty() {
                dropped.sort_unstable();
                for i in dropped.into_iter().rev() {
                    let e = kept.remove(i);
                    Self::event(
                        &tx,
                        &e.alias,
                        "turn_adopt_refused",
                        json!({"message": e.message_id, "turn_id": e.turn_id,
                               "reason": "recorded pane facts disagree across the alias"}),
                    )?;
                    refused.push(e.message_id.clone());
                }
            }
        }
        {
            let mut adoptions = self.adoptions.lock().unwrap();
            for e in &kept {
                adoptions
                    .entry(e.alias.clone())
                    .or_default()
                    .push((*e).clone());
            }
        }
        // CAD-250: a nudge is steering for the moment it was sent — it is
        // never replayed into a later daemon's pane.
        Self::cancel_nudges_in(&tx, None, "restart", None)?;
        // Dynamic NOT IN for the protected message ids — one UPDATE
        // either way, never string-interpolated values.
        let kept_ids: Vec<String> = kept.iter().map(|e| e.message_id.clone()).collect();
        // CAD-694: select the rows this recovery fences before the
        // UPDATE — a `failed` marker must emit each a refusal first,
        // and the outcome record needs the row list plus which of them
        // carry no refusal evidence at all. No `source` filter: this
        // list must equal exactly what the UPDATE below rewrites.
        let swept: Vec<(String, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT alias, id, turn_id FROM messages
                 WHERE state IN ('submitting','running')",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            rows.into_iter()
                .filter(|(_, id, _)| !kept_ids.contains(id))
                .collect()
        };
        // A `failed` marker means the previous run's
        // `shutdown_entries` never committed — no refusal events exist
        // for the rows it left in flight. Emit each the same refusal a
        // recorded refusal carries, so a restart landing on a
        // now-working store cannot read this silent sweep as clean.
        if let Some(failed) = marker.and_then(|m| m.failed.as_deref()) {
            for (alias, message_id, turn_id) in &swept {
                if refused.contains(message_id) {
                    continue;
                }
                Self::event(
                    &tx,
                    alias,
                    "turn_adopt_refused",
                    json!({"message": message_id, "turn_id": turn_id,
                           "reason":
                               format!("shutdown evidence failed ({failed}); the turn's fate is unproven — inspect and do not replay")}),
                )?;
                refused.push(message_id.clone());
            }
        }
        // Every swept row is fenced — `refused` carries this tx's
        // refusals and a committed drain wrote the rest. What remains
        // (a crash, or a drain that lost its writes entirely) is
        // unevidenced; the outcome reports both so a restart verdict
        // needing no per-alias cursor can still fail. The evidence
        // lookup is ONE chunked scan, not a query per row: the events
        // index is on job/seq, so a per-row EXISTS rescans the whole
        // history for every swept turn.
        let unprobed: Vec<&String> = swept
            .iter()
            .filter(|(_, id, _)| !refused.contains(id))
            .map(|(_, id, _)| id)
            .collect();
        let mut evidenced: std::collections::HashSet<String> = std::collections::HashSet::new();
        for chunk in unprobed.chunks(500) {
            let marks = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let mut stmt = tx.prepare(&format!(
                "SELECT DISTINCT json_extract(payload,'$.message') FROM events
                 WHERE kind='turn_adopt_refused'
                   AND json_extract(payload,'$.message') IN ({marks})"
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                r.get::<_, String>(0)
            })?;
            for row in rows {
                evidenced.insert(row?);
            }
        }
        let unevidenced: Vec<(String, String)> = swept
            .iter()
            .filter(|(_, id, _)| !refused.contains(id) && !evidenced.contains(id))
            .map(|(alias, id, _)| (alias.clone(), id.clone()))
            .collect();
        let outcome = RecoveryOutcome {
            marker_instance: marker.map(|m| m.instance.clone()),
            stale: marker.and_then(|m| m.stale.clone()),
            failed: marker.and_then(|m| m.failed.clone()),
            fenced: swept
                .iter()
                .map(|(a, id, _)| (a.clone(), id.clone()))
                .collect(),
            unevidenced,
        };
        let placeholders = kept_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut sql = String::from(
            "UPDATE messages SET state='unknown',
                error='Runtime restarted during provider turn'
             WHERE state IN ('submitting','running')",
        );
        if !kept_ids.is_empty() {
            sql.push_str(&format!(" AND id NOT IN ({placeholders})"));
        }
        tx.execute(&sql, rusqlite::params_from_iter(kept_ids.iter()))?;
        // An `attention` row is a fence, not a liveness state — keep the
        // state and its recorded error intact (they are the operator's
        // recovery context) and clear only the dead runtime fields.
        // Rewriting it to `offline` here would hide the fence from the
        // serve loop's relaunch skip and retry a provider session the
        // operator has not cleared.
        tx.execute(
            "UPDATE agents SET pid=NULL, pid_start=NULL, endpoint=NULL, generation=NULL
             WHERE state='attention' AND endpoint_kind != 'inbox'",
            [],
        )?;
        // Adopted agents lose `generation` with every other runtime
        // field — the token gate treats NULL as stale, so a report
        // landing between store open and the actor's pane proof is
        // rejected rather than finishing a turn whose pane may be
        // gone. `set_identity_adopted` writes the recorded generation
        // back once `open_adopted` has verified the pane, restoring
        // token validity. Kept aliases need the same clearing, so this
        // is one unconditional UPDATE — the crash path's exact shape.
        tx.execute(
            "UPDATE agents SET state='offline', pid=NULL, pid_start=NULL, endpoint=NULL,
                generation=NULL
             WHERE state NOT IN ('stopped','attention')
               AND endpoint_kind != 'inbox'",
            [],
        )?;
        // CAD-506 (ADR 0006 §5.4 step 8): a pending effect the last run
        // proved `decided`/`executing` but never reached an outcome may
        // already have fired — reconcile for a human, never re-fire.
        // `waiting` rows keep their state; they list from the table, so
        // nothing needs re-parking.
        self.reconcile_effects_in(&tx)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// CAD-162: `token` is current for `generation` under the alias's
    /// own endpoint scheme — [`registry::turn_token_current`] with the
    /// pair read in the caller's transaction. An alias with no agent
    /// row has no scheme, so nothing is current for it (fail closed).
    fn turn_token_current_in(tx: &Connection, alias: &str, generation: &str, token: &str) -> bool {
        tx.query_row(
            "SELECT provider, endpoint_kind FROM agents WHERE alias=?",
            [alias],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok()
        .is_some_and(|(provider, kind)| {
            registry::turn_token_current(&provider, &kind, Some(generation), token)
        })
    }

    /// Why a recorded shutdown entry cannot be adopted at the store
    /// level — `None` means the message may stay `running` for the
    /// actor's pane checks. Every failure maps to the plain recovery
    /// path (message `unknown`, agent fenced) for that agent only.
    fn adoption_block(&self, tx: &Connection, e: &AdoptEntry) -> Option<String> {
        let msg = tx
            .query_row(
                "SELECT state, turn_id FROM messages WHERE id=?",
                [&e.message_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .ok();
        let Some((state, turn_id)) = msg else {
            return Some(format!("message {} no longer exists", e.message_id));
        };
        if state != "running" {
            return Some(format!("message {} is {state}, not running", e.message_id));
        }
        if turn_id.as_deref() != Some(e.turn_id.as_str()) {
            return Some(format!("turn id for {} changed", e.message_id));
        }
        let agent = tx
            .query_row(
                "SELECT enabled, state FROM agents WHERE alias=?",
                [&e.alias],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        if agent.is_none() {
            return Some(format!("agent {} is gone", e.alias));
        }
        // Marker-internal consistency: a token that is not current for
        // the recorded generation under the agent's own endpoint scheme
        // (CAD-162) can never validate again — a marker this
        // inconsistent is corrupt or hand-built, so the turn fences.
        if !Self::turn_token_current_in(tx, &e.alias, &e.generation, &e.turn_id) {
            return Some(format!(
                "turn {} does not match recorded generation",
                e.message_id
            ));
        }
        match agent {
            None => Some(format!("agent {} is gone", e.alias)),
            Some((0, _)) => Some(format!("agent {} was disabled at shutdown", e.alias)),
            Some((_, s)) if s == "attention" => {
                Some(format!("agent {} was already fenced", e.alias))
            }
            Some(_) => {
                let unknown: i64 = tx
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM messages
                             WHERE alias=? AND {FENCING_UNKNOWN_SQL}"
                        ),
                        [&e.alias],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                (unknown > 0).then(|| format!("agent {} carries unreconciled unknowns", e.alias))
            }
        }
    }

    /// The adoption candidates `recover()` kept for this alias — every
    /// in-flight turn that qualified — consumed once by the actor at
    /// open; a second actor generation can never see them.
    pub fn take_adoption(&self, alias: &str) -> Option<Vec<AdoptEntry>> {
        self.adoptions
            .lock()
            .unwrap()
            .remove(alias)
            .filter(|v| !v.is_empty())
    }

    /// The endpoint facts a hot restart needs per pty alias — recorded
    /// generation, pane pid, native session — read while the endpoint
    /// is still live on the agent row (before detach clears them).
    /// `shutdown_entries` joins these against the final in-flight
    /// messages.
    pub fn pty_endpoint_facts(
        &self,
    ) -> Result<std::collections::HashMap<String, (String, u32, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT alias, generation, pid, session_id FROM agents
             WHERE endpoint_kind='pty'
               AND generation IS NOT NULL AND pid IS NOT NULL",
        )?;
        let facts = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? as u32,
                        r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    ),
                ))
            })?
            .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?;
        Ok(facts)
    }

    /// The registered pty panes a caller can descend from — the rows
    /// [`Self::pty_endpoint_facts`] reads — as `(alias, pane pid,
    /// recorded start time)` for [`crate::peer::AgentPids::classify`]:
    /// every pid → alias mapping checks the start (CAD-385).
    pub fn pty_pane_pids(&self) -> Result<Vec<(String, u32, Option<u64>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT alias, pid, pid_start FROM agents
             WHERE endpoint_kind='pty'
               AND generation IS NOT NULL AND pid IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(alias, pid, start)| {
                Some((
                    alias,
                    u32::try_from(pid).ok()?,
                    start.and_then(|s| u64::try_from(s).ok()),
                ))
            })
            .collect())
    }

    /// The shutdown marker's payload: every in-flight pty message
    /// joined against the endpoint facts captured while the pane was
    /// still live (`pty_endpoint_facts`). `submitting` rows ride along
    /// so recovery can name *why* they fenced — an unproven paste is
    /// never adoptable; it simply cannot satisfy the `running` check.
    /// Called only after all actors have detached — the message rows
    /// are final, while the facts come from the earlier snapshot since
    /// detach has already cleared them from the agent rows.
    ///
    /// A pty alias missing from `facts` is named, not skipped. The
    /// snapshot is taken before actors wake, but a row that already
    /// had no live `pid`/`generation` cannot be adopted. Omitting it
    /// made recovery fence every in-flight turn of that alias with no
    /// per-turn evidence. Non-pty rows are not adoption candidates and
    /// stay unnamed.
    ///
    /// A `running` row whose token does not embed the snapshot
    /// generation is skipped and named: the facts were captured when
    /// shutdown was requested, while RPC threads were still live, so a
    /// resume racing the stop could re-open the pane under a newer
    /// generation and leave this token stale forever. Recording it
    /// would roll the agent row back to the old generation — instead
    /// the row is refused now and fences on restart like any other
    /// refusal.
    ///
    /// The error's cause is typed at its source: [`ShutdownDrainError::Fenced`]
    /// only when the write fence refused the write, so a caller never
    /// re-reads the (mutable, time-dependent) fence to guess why a
    /// drain failed.
    pub fn shutdown_entries(
        &self,
        facts: &std::collections::HashMap<String, (String, u32, String)>,
    ) -> std::result::Result<Vec<AdoptEntry>, ShutdownDrainError> {
        // The drain must not lose its adoption evidence to a racing
        // writer: a rollout/audit commit landing between the in-flight
        // scan and the event writes fails the write upgrade as BUSY —
        // including BUSY_SNAPSHOT, which busy_timeout cannot wait out.
        // Re-run the whole transaction on the busy family; a dead store
        // fails every attempt just as fast, so nothing else is retried.
        let mut attempt = 0u32;
        loop {
            let result = {
                // `write_conn` fails only on the fence: that refusal is
                // the one typed-fenced cause.
                let conn = self.write_conn().map_err(ShutdownDrainError::Fenced)?;
                self.shutdown_entries_tx(&conn, facts)
            };
            match result {
                Ok(entries) => return Ok(entries),
                Err(e) if attempt < SHUTDOWN_ENTRIES_RETRIES && shutdown_retryable(&e) => {
                    attempt += 1;
                    eprintln!(
                        "store: shutdown entries hit {e}; retrying ({attempt}/{SHUTDOWN_ENTRIES_RETRIES})"
                    );
                    std::thread::sleep(Duration::from_millis(
                        self.shutdown_backoff_ms * u64::from(attempt),
                    ));
                }
                Err(e) => return Err(ShutdownDrainError::Failed(e.into())),
            }
        }
    }

    /// One [`Self::shutdown_entries`] transaction, rusqlite-typed so
    /// the caller can tell retryable lock contention from a dead store.
    fn shutdown_entries_tx(
        &self,
        conn: &Connection,
        facts: &std::collections::HashMap<String, (String, u32, String)>,
    ) -> rusqlite::Result<Vec<AdoptEntry>> {
        // IMMEDIATE: take the write lock up front so `busy_timeout`
        // waits for it. A deferred transaction upgrades its read lock
        // on the first write and gets SQLITE_BUSY at once, burning a
        // retry on contention the lock wait would have absorbed. The
        // bounded retry stays as the backstop.
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        let mut stmt = tx.prepare(
            "SELECT alias, id, turn_id, state FROM messages
             WHERE state IN ('running','submitting') AND source != 'nudge'",
        )?;
        let inflight = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut entries = Vec::new();
        for (alias, message_id, turn_id, state) in inflight {
            let Some((generation, pane_pid, native_session)) = facts.get(&alias) else {
                let kind: Option<String> = tx
                    .query_row(
                        "SELECT endpoint_kind FROM agents WHERE alias=?",
                        [&alias],
                        |r| r.get(0),
                    )
                    .ok();
                if kind.as_deref() == Some("pty") {
                    Self::event_scoped_raw(
                        &tx,
                        &alias,
                        "turn_adopt_refused",
                        &json!({"message": message_id, "turn_id": turn_id,
                               "reason": "endpoint identity was not provable at shutdown; inspect the pane and do not replay"}),
                        None,
                        None,
                    )?;
                }
                continue;
            };
            if state == "running" && !Self::turn_token_current_in(&tx, &alias, generation, &turn_id)
            {
                Self::event_scoped_raw(
                    &tx,
                    &alias,
                    "turn_adopt_refused",
                    &json!({"message": message_id, "turn_id": turn_id,
                           "reason": "turn token predates endpoint generation"}),
                    None,
                    None,
                )?;
                continue;
            }
            entries.push(AdoptEntry {
                alias,
                message_id,
                turn_id,
                generation: generation.clone(),
                pane_pid: *pane_pid,
                native_session: native_session.clone(),
            });
        }
        // Test seam (CAD-694): the hook runs inside this attempt's
        // transaction AFTER its production writes — a synthetic error
        // here discards them too, so rollback coverage is the real
        // shape, not just pre-write faults. Never set in production.
        if let Some(hook) = &self.shutdown_entries_hook {
            hook(&tx)?;
        }
        tx.commit()?;
        Ok(entries)
    }
}

/// Why [`Store::shutdown_entries`] failed, decided where the error is
/// produced (CAD-694): the lease fence refusing the write, or a fault
/// of the drain itself.
#[derive(Debug)]
pub enum ShutdownDrainError {
    /// The hosted-lease fence refused the write — the fence's own
    /// consequence, not a new fault (CAD-538).
    Fenced(crate::Error),
    /// The store failed the drain: a real fault, whatever the fence
    /// says afterwards.
    Failed(crate::Error),
}

impl ShutdownDrainError {
    pub fn is_fenced(&self) -> bool {
        matches!(self, Self::Fenced(_))
    }
}

impl std::fmt::Display for ShutdownDrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fenced(e) | Self::Failed(e) => e.fmt(f),
        }
    }
}

/// sqlite errors worth a fresh shutdown transaction: BUSY — whose
/// SNAPSHOT variant busy_timeout cannot wait out at all — and LOCKED.
/// Anything else (I/O, full disk, schema, misuse) is not transient.
fn shutdown_retryable(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// Extra `shutdown_entries` attempts on lock contention — two retries
/// after the first try, at most three transactions; each pays the
/// connection's busy_timeout (~5s) only while contention lasts, so a
/// store that stays wedged adds ~15s to the stop before it fails loud.
const SHUTDOWN_ENTRIES_RETRIES: u32 = 2;
