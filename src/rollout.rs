//! Rollout lease (CAD-268).
//!
//! One active lease, held in the daemon's database, gates `daemon restart`
//! and any schema crossing. Callers inside a cadence pane are
//! `$CADENCE_ALIAS`. Everyone else must pass `--as <identity>`. A path
//! or `HEAD` is never an identity.
//!
//! The lease table is created by schema 12. A database that is still on
//! schema 11 has no lease table, and the binary that wrote it has no
//! `cadence rollout` command, so it cannot record a backup receipt.
//! `authorize_migration` therefore refuses that crossing by default
//! (`no table` = `no lease`) and leaves the file untouched. The single
//! introduction of this feature is the explicit opt-in
//! `CADENCE_ROLLOUT_BOOTSTRAP=1`, which allows only schema 11 → 12 when
//! `rollout_leases` is absent. It does not authorize any later crossing.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::peer::proc_starttime;
use crate::store::{self, Store};

/// Current store schema. v12 introduced `rollout_leases` and
/// `daemon_build`; v13 adds conversation threads (CAD-319); v14 adds
/// `agents.pid_start`, the recorded pid's process start time (CAD-385);
/// v15 adds `messages.priority` (CAD-158); v16 adds
/// `messages.issue`/`messages.worktree`, the dispatch lane a kickoff
/// belongs to (CAD-467); v17 adds the platform custody, grant and
/// project-default tables (CAD-366); v18 adds the pending-effect and
/// draft tables (CAD-506); v19 adds `app_grants.install_id`, the
/// install an app-derived grant belongs to (CAD-577).
/// The newest migration in `store` writes this number.
pub const SCHEMA_VERSION: i64 = 19;

/// Last schema that has no lease table. The bootstrap opt-in covers
/// only this version.
pub const PRE_LEASE_SCHEMA: i64 = 11;

const DEFAULT_TTL: Duration = Duration::from_secs(12 * 60 * 60);

thread_local! {
    /// Test override so a bootstrap opt-in does not leak across threads
    /// through the process environment.
    static BOOTSTRAP_FORCE: Cell<Option<bool>> = const { Cell::new(None) };
    /// Test override for the identity allowed to migrate. Unset in
    /// production; the daemon reads `CADENCE_ROLLOUT_AS` then
    /// `CADENCE_ALIAS`.
    static MIGRATION_HOLDER: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Identity passed on `daemon run --rollout-as`. Process-wide, not
/// thread-local: `new_hot` must see it even if a later refactor moves
/// that call off the main thread. Set once, from `main`, before any
/// worker starts. Kept out of the environment so panes do not inherit it.
static FORWARDED_AS: OnceLock<String> = OnceLock::new();

/// Remember the holder forwarded by `daemon start` for this process.
pub fn set_forwarded_identity(identity: Option<String>) {
    remember_forwarded(&FORWARDED_AS, identity);
}

/// Set `slot` once. Tests pass a local lock so a run of
/// `cargo test --lib rollout` does not stick a process-wide identity
/// that later tests cannot clear.
fn remember_forwarded(slot: &OnceLock<String>, identity: Option<String>) {
    let Some(identity) = identity
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let _ = slot.set(identity);
}

fn forwarded_identity() -> Option<String> {
    read_forwarded(&FORWARDED_AS)
}

fn read_forwarded(slot: &OnceLock<String>) -> Option<String> {
    slot.get().cloned()
}

pub fn default_ttl() -> Duration {
    DEFAULT_TTL
}

fn bootstrap_requested() -> bool {
    if let Some(forced) = BOOTSTRAP_FORCE.with(Cell::get) {
        return forced;
    }
    std::env::var("CADENCE_ROLLOUT_BOOTSTRAP").ok().as_deref() == Some("1")
}

/// Identity allowed to migrate: the test override, else the identity
/// forwarded into this process, else the pane alias.
fn caller_for_migration() -> Option<String> {
    let forced = MIGRATION_HOLDER.with(|slot| slot.borrow().clone());
    if forced.is_some() {
        return forced;
    }
    forwarded_identity()
        .or_else(|| env_nonempty("CADENCE_ROLLOUT_AS"))
        .or_else(|| env_nonempty("CADENCE_ALIAS"))
}

/// Caller inside `daemon run`: the argv identity wins, so a pane alias
/// inherited from the parent cannot veto the holder who started it.
fn process_caller() -> Result<Caller> {
    if let Some(identity) = forwarded_identity() {
        validate_identity(&identity)?;
        return Ok(Caller {
            identity,
            source: "as",
        });
    }
    resolve_caller(None)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
const MAX_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const FORCE_RELEASE_HINT: &str = "An expired holder is cleared with \
     `cadence rollout release --force --reason \"<why>\" --as operator:<name>`. \
     A lease that has not expired also requires `--holder <that identity>`.";
const GATE_LOG: &str = "rollout-gate.jsonl";

/// A crossing `authorize_migration` has allowed but not yet recorded.
#[derive(Debug, Clone)]
pub struct SchemaCrossing {
    pub from: i64,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct MigrationPermit {
    pub crossing: Option<SchemaCrossing>,
}

#[derive(Debug, Clone)]
pub struct Caller {
    pub identity: String,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub struct RestartTicket {
    pub lease_id: i64,
    pub holder: String,
}

/// A transaction outcome that must commit even when the caller is refused,
/// so the refusal event and any expiry mark survive.
enum TxResult<T> {
    Done(T),
    Refuse(String),
}

fn committed<T>(result: Result<TxResult<T>>) -> Result<T> {
    match result {
        Ok(TxResult::Done(value)) => Ok(value),
        Ok(TxResult::Refuse(message)) => Err(Error::rejected(message)),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone)]
struct Lease {
    id: i64,
    holder: String,
    holder_source: String,
    host_note: String,
    reason: String,
    target_commit: Option<String>,
    schema_from: Option<i64>,
    schema_to: Option<i64>,
    backup_path: Option<String>,
    backup_sha256: Option<String>,
    backup_schema: Option<i64>,
    backup_taken_at: Option<f64>,
    claimed_at: f64,
    expires_at: f64,
    status: String,
    previous_holder: Option<String>,
}

pub fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn db_file(state_dir: &Path) -> PathBuf {
    state_dir.join("cadence.sqlite3")
}

/// The store's schema version, read without writing the database:
/// `None` when there is no database yet (or it is fresh), so
/// `cadence update --check` can say whether a migration is involved.
pub fn store_schema(state_dir: &Path) -> Result<Option<i64>> {
    match peek_schema(&db_file(state_dir))? {
        Peek::Version(v) => Ok(Some(v)),
        Peek::Missing | Peek::Fresh => Ok(None),
    }
}

pub fn parse_ttl(raw: &str) -> Result<Duration> {
    let raw = raw.trim();
    let (num, mult) = if let Some(rest) = raw.strip_suffix('d') {
        (rest, 86_400u64)
    } else if let Some(rest) = raw.strip_suffix('h') {
        (rest, 3_600)
    } else if let Some(rest) = raw.strip_suffix('m') {
        (rest, 60)
    } else if let Some(rest) = raw.strip_suffix('s') {
        (rest, 1)
    } else {
        (raw, 1)
    };
    let n: u64 = num
        .parse()
        .map_err(|_| Error::rejected(format!("invalid ttl '{raw}' (use 90s, 30m, 12h, or 1d)")))?;
    if n == 0 {
        return Err(Error::rejected("ttl must be at least 1 second"));
    }
    let secs = n
        .checked_mul(mult)
        .ok_or_else(|| Error::rejected("ttl is too large"))?;
    let ttl = Duration::from_secs(secs);
    if ttl > MAX_TTL {
        return Err(Error::rejected("ttl cannot exceed 7 days"));
    }
    Ok(ttl)
}

pub fn resolve_caller(explicit_as: Option<&str>) -> Result<Caller> {
    let alias = env_nonempty("CADENCE_ALIAS");
    let forwarded = explicit_as
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(forwarded_identity)
        .or_else(|| env_nonempty("CADENCE_ROLLOUT_AS"));
    resolve_caller_with(alias.as_deref(), forwarded.as_deref())
}

pub fn resolve_caller_with(alias: Option<&str>, explicit_as: Option<&str>) -> Result<Caller> {
    let alias = alias.map(str::trim).filter(|s| !s.is_empty());
    let explicit = explicit_as.map(str::trim).filter(|s| !s.is_empty());
    match (alias, explicit) {
        (Some(alias), Some(explicit)) if alias != explicit => Err(Error::rejected(
            "inside a cadence pane the lease holder is $CADENCE_ALIAS; \
             --as must match it. Ownership is never taken from a path or HEAD",
        )),
        (Some(alias), _) => {
            validate_identity(alias)?;
            Ok(Caller {
                identity: alias.to_string(),
                source: "alias",
            })
        }
        (None, Some(explicit)) => {
            validate_identity(explicit)?;
            Ok(Caller {
                identity: explicit.to_string(),
                source: "as",
            })
        }
        (None, None) => Err(Error::rejected(
            "no rollout identity: inside a cadence pane the holder is \
             $CADENCE_ALIAS; outside a pane pass --as <identity> \
             (for example --as operator:name). Ownership is never inferred \
             from a path or HEAD",
        )),
    }
}

fn validate_identity(identity: &str) -> Result<()> {
    if identity.len() > 200
        || identity
            .chars()
            .any(|c| c.is_control() || c == '\n' || c == '\r')
    {
        return Err(Error::rejected(
            "identity must be 1..=200 characters with no control characters",
        ));
    }
    Ok(())
}

/// Refuse a schema crossing unless the database is fresh, already
/// current, or an active lease holds a matching backup receipt.
/// Does not open the database for writing.
pub fn authorize_migration(path: &Path) -> Result<MigrationPermit> {
    let peek = peek_schema(path)?;
    let version = match peek {
        Peek::Missing | Peek::Fresh => {
            return Ok(MigrationPermit { crossing: None });
        }
        Peek::Version(v) if v == SCHEMA_VERSION => {
            return Ok(MigrationPermit { crossing: None });
        }
        Peek::Version(v) if v > SCHEMA_VERSION => {
            return Err(Error::rejected(format!(
                "database schema {v} is newer than this binary's schema {SCHEMA_VERSION}; refusing to open"
            )));
        }
        Peek::Version(v) => v,
    };
    if path.parent().is_some_and(sandbox_exempt) {
        return Ok(MigrationPermit { crossing: None });
    }

    let bootstrap = bootstrap_requested();
    let table = lease_table_present(path)?;
    if bootstrap && version == PRE_LEASE_SCHEMA && !table {
        return Ok(MigrationPermit {
            crossing: Some(SchemaCrossing {
                from: version,
                reason: "lease_table_introduction".into(),
            }),
        });
    }

    if !qualifying_receipt(path, version, caller_for_migration().as_deref())? {
        append_gate_log(
            path,
            "rollout_migration_refused",
            json!({
                "from": version,
                "to": SCHEMA_VERSION,
                "reason": "no matching backup receipt",
            }),
        )?;
        return Err(Error::rejected(migration_refusal(version, table)));
    }
    Ok(MigrationPermit {
        crossing: Some(SchemaCrossing {
            from: version,
            reason: "backup_receipt".into(),
        }),
    })
}

fn migration_refusal(version: i64, lease_table: bool) -> String {
    let bootstrap = if version == PRE_LEASE_SCHEMA && !lease_table {
        " This database has no rollout_leases table, so the previous binary \
         cannot run `cadence rollout`. For this single introduction of the \
         lease table (schema 11 upward), start the daemon once with \
         CADENCE_ROLLOUT_BOOTSTRAP=1. \
         That opt-in does not authorize any later schema change."
    } else {
        ""
    };
    format!(
        "refusing to migrate schema {version} to {SCHEMA_VERSION}: no active \
         rollout lease holds a backup receipt taken after the claim whose \
         schema_version is {version}. The database was not modified. Take a \
         copy of the sqlite file, then run `cadence rollout claim --reason \
         \"<why>\" --as <identity>` and `cadence rollout backup --path \
         <backup-file>` with a binary that already has the lease commands, \
         then retry.{bootstrap}"
    )
}

pub fn ensure_lease_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rollout_leases(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            holder TEXT NOT NULL,
            holder_source TEXT NOT NULL,
            host_note TEXT NOT NULL DEFAULT '',
            reason TEXT NOT NULL,
            target_commit TEXT,
            schema_from INTEGER,
            schema_to INTEGER,
            backup_path TEXT,
            backup_sha256 TEXT,
            backup_schema INTEGER,
            backup_taken_at REAL,
            claimed_at REAL NOT NULL,
            expires_at REAL NOT NULL,
            status TEXT NOT NULL,
            previous_holder TEXT,
            ended_at REAL,
            end_reason TEXT);
         CREATE UNIQUE INDEX IF NOT EXISTS rollout_lease_one_active
            ON rollout_leases(status) WHERE status = 'active';
         CREATE TABLE IF NOT EXISTS daemon_build(
            id INTEGER PRIMARY KEY CHECK (id = 1),
            commit_sha TEXT NOT NULL,
            recorded_at REAL NOT NULL);
         CREATE TABLE IF NOT EXISTS rollout_grants(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            alias TEXT NOT NULL,
            granted_by TEXT NOT NULL,
            granted_at REAL NOT NULL,
            expires_at REAL,
            revoked_at REAL,
            revoked_by TEXT);",
    )?;
    Ok(())
}

pub fn upsert_daemon_build(conn: &Connection, commit: &str, at: f64) -> Result<()> {
    ensure_lease_tables(conn)?;
    conn.execute(
        "INSERT INTO daemon_build(id, commit_sha, recorded_at) VALUES(1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET commit_sha = excluded.commit_sha,
                                      recorded_at = excluded.recorded_at",
        params![commit, at],
    )?;
    Ok(())
}

/// `daemon start` / `daemon restart`'s spawn. A missing database or a
/// recorded commit equal to this binary is allowed without a lease.
/// A different commit requires the caller to hold the active lease.
/// Returns the identity to forward to the child, when one was resolved.
pub fn authorize_daemon_spawn(
    state_dir: &Path,
    explicit_as: Option<&str>,
) -> Result<Option<String>> {
    authorize_spawn_for(state_dir, resolve_caller(explicit_as))
}

/// Read-only build and holder check for a direct `daemon run`.
/// Call before the hot-restart marker is consumed and before the store
/// opens the database. A refused run leaves `shutdown.json` and the
/// sqlite family untouched.
pub fn authorize_direct_run(state_dir: &Path) -> Result<()> {
    authorize_spawn_for(state_dir, process_caller()).map(|_| ())
}

/// A sandbox's own state dir (`<root>/state` beside its marker) takes
/// no part in a rollout: it is disposable, so a rebuilt binary starts
/// it, and migrates it, without the lease (CAD-310).
pub fn sandbox_exempt(state_dir: &Path) -> bool {
    matches!(crate::sandbox::owner_of(state_dir), Ok(Some(_)))
}

pub fn authorize_spawn_for(state_dir: &Path, caller: Result<Caller>) -> Result<Option<String>> {
    if sandbox_exempt(state_dir) {
        return Ok(caller.ok().map(|c| c.identity));
    }
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(caller.ok().map(|c| c.identity));
    }
    // Read-only peek. A refused start must not open the live file
    // read-write (that creates `-wal`/`-shm` and can checkpoint).
    let peek = open_peek(&path)?;
    let recorded = read_build(&peek.conn).map_err(|error| {
        Error::rejected(format!(
            "daemon start refused: could not read the recorded build ({error})"
        ))
    })?;
    let current = crate::overview::BUILD_COMMIT;
    if recorded.is_none() || recorded.as_deref() == Some(current) {
        return Ok(caller.ok().map(|c| c.identity));
    }
    let caller = caller?;
    match holder_block(&peek.conn, &caller, unix_now())? {
        None => Ok(Some(caller.identity)),
        Some(block) => {
            let message = format!(
                "daemon start of build {current} refused: the daemon last \
                 recorded build {} and {block}",
                recorded.clone().unwrap_or_default()
            );
            // Side log, not a read-write open. Opening the live file here
            // created `-wal`/`-shm` and could checkpoint a crash-left WAL.
            // The next successful start ingests this line and stamps
            // `source: gate_log`.
            let _ = append_gate_log(
                &path,
                "rollout_start_refused",
                json!({
                    "build": current,
                    "recorded": recorded.unwrap_or_default(),
                    "holder": caller.identity,
                    "reason": block,
                }),
            );
            Err(Error::rejected(message))
        }
    }
}

/// Backstop inside `daemon run` for a binary started without `daemon start`.
pub fn enforce_running_build(conn: &Connection) -> Result<()> {
    let recorded = read_build(conn).map_err(|error| {
        Error::rejected(format!(
            "daemon run refused: could not read the recorded build ({error})"
        ))
    })?;
    let current = crate::overview::BUILD_COMMIT;
    if recorded.is_none() || recorded.as_deref() == Some(current) {
        return Ok(());
    }
    let now = unix_now();
    let caller = process_caller();
    let allowed = match &caller {
        Ok(caller) => holder_block(conn, caller, now)?.is_none(),
        Err(_) => false,
    };
    if allowed {
        let _ = insert_event(
            conn,
            "rollout_start_proceeded",
            json!({
                "build": current,
                "recorded": recorded,
                "holder": caller.ok().map(|c| c.identity),
            }),
            now,
        );
        return Ok(());
    }
    let why = match caller {
        Ok(caller) => {
            holder_block(conn, &caller, now)?.unwrap_or_else(|| "lease check failed".into())
        }
        Err(e) => e.to_string(),
    };
    let message = format!(
        "daemon run of build {current} refused: the daemon last recorded \
         build {} and {why}",
        recorded.clone().unwrap_or_default()
    );
    let _ = insert_event(
        conn,
        "rollout_start_refused",
        json!({"build": current, "recorded": recorded, "reason": why}),
        now,
    );
    Err(Error::rejected(message))
}

pub fn begin_restart(state_dir: &Path, caller: &Caller) -> Result<RestartTicket> {
    let path = db_file(state_dir);
    if !path.exists() {
        return Err(Error::rejected(
            "daemon restart refused before shutdown: no rollout lease is held. \
             Run `cadence rollout claim --reason \"<why>\" --as <identity>`",
        ));
    }
    let conn = connect(&path)?;
    let now = unix_now();
    if let Some(block) = holder_block(&conn, caller, now)? {
        let _ = insert_event(
            &conn,
            "rollout_restart_refused",
            json!({"phase": "initial", "holder": caller.identity, "reason": block}),
            now,
        );
        return Err(Error::rejected(format!(
            "daemon restart refused before shutdown: {block}"
        )));
    }
    let lease = active_lease(&conn)?.expect("holder_block found the lease");
    Ok(RestartTicket {
        lease_id: lease.id,
        holder: lease.holder,
    })
}

pub fn recheck_restart(state_dir: &Path, ticket: &RestartTicket) -> Result<()> {
    let conn = connect(&db_file(state_dir))?;
    let now = unix_now();
    let row = lease_by_id(&conn, ticket.lease_id)?;
    let block = match row {
        Some(lease)
            if lease.status == "active"
                && lease.holder == ticket.holder
                && lease.expires_at > now =>
        {
            None
        }
        Some(lease) if lease.status == "active" && lease.expires_at <= now => {
            // Leave the row active so a later `--takeover` still names
            // this holder. Ending it here made the refusal's own advice fail.
            Some(format!(
                "rollout lease expired while waiting for the idle gate (holder {}, expires_at {}); pass --takeover to claim it",
                lease.holder,
                fmt_epoch(lease.expires_at)
            ))
        }
        Some(lease) if lease.status == "released" => {
            Some("rollout lease was released while waiting for the idle gate".into())
        }
        Some(lease) if lease.status == "handed_off" => {
            let to = active_lease(&conn)?
                .map(|l| l.holder)
                .unwrap_or_else(|| "unknown".into());
            Some(format!(
                "rollout lease was handed off to {to} while waiting for the idle gate"
            ))
        }
        Some(lease) if lease.status == "taken_over" => {
            let to = active_lease(&conn)?
                .map(|l| l.holder)
                .unwrap_or_else(|| "unknown".into());
            Some(format!(
                "rollout lease was taken over by {to} while waiting for the idle gate"
            ))
        }
        Some(lease) => Some(format!(
            "rollout lease is no longer held by {} (status {})",
            ticket.holder, lease.status
        )),
        None => Some("rollout lease disappeared while waiting for the idle gate".into()),
    };
    if let Some(block) = block {
        let _ = insert_event(
            &conn,
            "rollout_restart_refused",
            json!({"phase": "recheck", "holder": ticket.holder, "reason": block}),
            now,
        );
        return Err(Error::rejected(format!(
            "restart aborted before shutdown: {block}"
        )));
    }
    Ok(())
}

/// The holder of the active, unexpired rollout lease — when that
/// holder also holds a live operator grant (CAD-384). The daemon's
/// `shutdown` caller rule admits that agent's own pane: the rollout
/// owner restarts from its pane, but only because the operator granted
/// it (`cadence rollout grant`). A lease without a live grant — an
/// agent's handoff, a grant since revoked or expired — admits nobody.
/// Read-only; no lease table (or no database yet) is no holder.
pub fn granted_lease_holder(state_dir: &Path) -> Result<Option<String>> {
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(None);
    }
    let conn = connect(&path)?;
    let now = unix_now();
    let Some(lease) = active_lease(&conn)?.filter(|lease| lease.expires_at > now) else {
        return Ok(None);
    };
    Ok(live_grant(&conn, &lease.holder, now)?.map(|_| lease.holder))
}

/// The live grant for `alias` — not revoked, not expired — as
/// `(granted_by, granted_at, expires_at)`.
fn live_grant(
    conn: &Connection,
    alias: &str,
    now: f64,
) -> Result<Option<(String, f64, Option<f64>)>> {
    if !table_exists(conn, "rollout_grants")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT granted_by, granted_at, expires_at FROM rollout_grants
         WHERE alias=?1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?2)
         ORDER BY id DESC LIMIT 1",
        params![alias, now],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .map_err(Into::into)
}

/// `rollout grant` (CAD-384): the operator lets agent `alias` claim the
/// rollout lease — and, holding it, stop the daemon from its own pane.
/// The caller must already be proven the operator: the daemon's
/// `rollout_grant` RPC is the only caller. A new grant supersedes the
/// alias's live one. `until` is an absolute expiry (epoch seconds).
pub fn grant(state_dir: &Path, alias: &str, until: Option<f64>, by: &str) -> Result<Value> {
    validate_identity(alias)?;
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    if until.is_some_and(|at| at <= now) {
        return Err(Error::rejected("a grant's --until must be in the future"));
    }
    committed(immediate(&conn, |conn| {
        conn.execute(
            "UPDATE rollout_grants SET revoked_at=?1, revoked_by='superseded'
             WHERE alias=?2 AND revoked_at IS NULL",
            params![now, alias],
        )?;
        conn.execute(
            "INSERT INTO rollout_grants(alias,granted_by,granted_at,expires_at)
             VALUES(?1,?2,?3,?4)",
            params![alias, by, now, until],
        )?;
        let payload = json!({"alias": alias, "by": by, "expires_at": until});
        insert_event(conn, "rollout_grant", payload.clone(), now)?;
        Ok(TxResult::Done(json!({
            "granted": alias, "by": by, "granted_at": now, "expires_at": until,
        })))
    }))
}

/// `rollout revoke` (CAD-384): end `alias`'s live grant. Its lease, if
/// it holds one, no longer admits its pane's `shutdown`, and it cannot
/// claim again. The operator only, like [`grant`].
pub fn revoke(state_dir: &Path, alias: &str, by: &str) -> Result<Value> {
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    committed(immediate(&conn, |conn| {
        let n = conn.execute(
            "UPDATE rollout_grants SET revoked_at=?1, revoked_by=?2
             WHERE alias=?3 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?1)",
            params![now, by, alias],
        )?;
        if n == 0 {
            return Ok(TxResult::Refuse(format!(
                "'{alias}' holds no live rollout grant"
            )));
        }
        insert_event(
            conn,
            "rollout_revoke",
            json!({"alias": alias, "by": by}),
            now,
        )?;
        Ok(TxResult::Done(
            json!({"revoked": alias, "by": by, "at": now}),
        ))
    }))
}

/// Live grants, for `rollout status`.
fn live_grants(conn: &Connection, now: f64) -> Result<Vec<Value>> {
    if !table_exists(conn, "rollout_grants")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT alias, granted_by, granted_at, expires_at FROM rollout_grants
         WHERE revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?1) ORDER BY alias",
    )?;
    let rows = stmt.query_map(params![now], |row| {
        Ok(json!({
            "alias": row.get::<_, String>(0)?,
            "by": row.get::<_, String>(1)?,
            "granted_at": row.get::<_, f64>(2)?,
            "expires_at": row.get::<_, Option<f64>>(3)?,
        }))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn note_restart_proceeded(state_dir: &Path, ticket: &RestartTicket) -> Result<()> {
    let conn = connect(&db_file(state_dir))?;
    insert_event(
        &conn,
        "rollout_restart_proceeded",
        json!({
            "holder": ticket.holder,
            "lease_id": ticket.lease_id,
            "build": crate::overview::BUILD_COMMIT,
        }),
        unix_now(),
    )
}

pub struct ClaimRequest<'a> {
    pub caller: &'a Caller,
    pub reason: &'a str,
    pub target: Option<&'a str>,
    pub ttl: Duration,
    pub takeover: bool,
    pub now: f64,
}

pub fn claim(state_dir: &Path, req: &ClaimRequest<'_>) -> Result<Value> {
    validate_reason(req.reason)?;
    if let Some(target) = req.target {
        validate_target(target)?;
    }
    // CAD-384: an operator-shaped holder (`--as operator:<name>`, no
    // pane alias) must BE the operator — otherwise an agent's
    // `env -u CADENCE_ALIAS … claim --as operator:x` holds the lease and
    // blocks the operator's own claim. Checked before any write, outside
    // the transaction (the proof peeks the same database).
    if req.caller.source == "as" {
        require_operator_proof(state_dir, "rollout claim --as")?;
    }
    let conn = connect_ensured(&db_file(state_dir))?;
    committed(immediate(&conn, |conn| claim_in(conn, req)))
}

pub fn status(state_dir: &Path) -> Result<Value> {
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(json!({"held": false}));
    }
    let conn = connect(&path)?;
    if !table_exists(&conn, "rollout_leases")? {
        return Ok(json!({"held": false}));
    }
    let now = unix_now();
    let mut view = committed(immediate(&conn, |conn| {
        if let Some(lease) = active_lease(conn)? {
            if lease.expires_at <= now {
                // Report the expiry without ending the row. Ending it
                // here made the following `--takeover` find no lease.
                let mut view = held_status(&lease, now);
                view["expired"] = json!(true);
                return Ok(TxResult::Done(view));
            }
            return Ok(TxResult::Done(held_status(&lease, now)));
        }
        Ok(TxResult::Done(json!({"held": false})))
    }))?;
    // CAD-384: who the operator lets claim the lease from a pane.
    let grants = live_grants(&conn, now)?;
    if !grants.is_empty() {
        view["grants"] = json!(grants);
    }
    Ok(view)
}

pub fn release(state_dir: &Path, caller: &Caller) -> Result<Value> {
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    committed(immediate(&conn, |conn| {
        let lease = match require_holder(conn, caller, now, false)? {
            TxResult::Done(lease) => lease,
            TxResult::Refuse(message) => return Ok(TxResult::Refuse(message)),
        };
        mark_ended(conn, lease.id, "released", "release", now)?;
        insert_event(
            conn,
            "rollout_release",
            json!({"holder": lease.holder, "lease_id": lease.id}),
            now,
        )?;
        Ok(TxResult::Done(
            json!({"released": true, "holder": lease.holder, "lease_id": lease.id}),
        ))
    }))
}

/// Renew the caller's own active lease to `ttl` from `now` — the holder
/// extending its TTL in place, without ending and re-claiming the lease
/// (CAD-561 r3: an update that reuses a same-identity lease must not let
/// it lapse mid-run, or the daemon stops adopting the update marker and
/// the drain re-assert is refused). Only the holder's own unexpired
/// lease renews; anyone else gets [`release`]'s refusal.
///
/// The renewal carries the new run's `reason` and `target` onto the
/// reused row: a crashed predecessor's lease was claimed for its own
/// reason, and `rollout status` would keep reporting the stale one
/// (CAD-561 r4).
pub fn renew(
    state_dir: &Path,
    caller: &Caller,
    reason: &str,
    target: Option<&str>,
    ttl: Duration,
    now: f64,
) -> Result<Value> {
    validate_reason(reason)?;
    if let Some(target) = target {
        validate_target(target)?;
    }
    let conn = connect_ensured(&db_file(state_dir))?;
    committed(immediate(&conn, |conn| {
        let lease = match require_holder(conn, caller, now, false)? {
            TxResult::Done(lease) => lease,
            TxResult::Refuse(message) => return Ok(TxResult::Refuse(message)),
        };
        let expires_at = now + ttl.as_secs_f64();
        conn.execute(
            "UPDATE rollout_leases SET expires_at=?1, reason=?2, target_commit=?3 \
             WHERE id=?4",
            params![expires_at, reason, target, lease.id],
        )?;
        insert_event(
            conn,
            "rollout_renew",
            json!({
                "holder": lease.holder,
                "lease_id": lease.id,
                "expires_at": expires_at,
                "previous_expires_at": lease.expires_at,
                "reason": reason,
                "target": target,
            }),
            now,
        )?;
        Ok(TxResult::Done(json!({
            "renewed": true,
            "holder": lease.holder,
            "lease_id": lease.id,
            "expires_at": expires_at,
        })))
    }))
}

/// Operator override for a holder who is gone. Does not require the
/// caller to be the holder. Records the ousted holder.
///
/// A live, unexpired lease is refused unless `ousted_holder` names that
/// holder. The caller must also pass [`crate::peer::operator_proof`] for
/// this process: its own pid and current uid, the registered pane pids
/// from a read-only peek, and enrolled roots from `slots.json` (a missing
/// file means nothing is enrolled). The proof reads `/proc` and those
/// inputs; it does not need a running daemon.
///
/// `daemon_pid` is the pid that holds the `cadence.lock` flock while a
/// daemon is running. When the daemon is stopped — the dead-holder
/// recovery path — the value is `0`. [`crate::adapter::pty::caller_chain`]
/// records only pids greater than 1, so `0` cannot match an ancestor and
/// the daemon-descendant hop does not apply.
pub fn release_forced(
    state_dir: &Path,
    caller: &Caller,
    reason: &str,
    ousted_holder: Option<&str>,
) -> Result<Value> {
    validate_reason(reason)?;
    if caller.source != "as" {
        return Err(Error::rejected(
            "rollout release --force requires an operator identity outside a cadence pane \
             (`--as operator:<name> --reason \"<why>\"`). A pane's $CADENCE_ALIAS cannot force-release.",
        ));
    }
    reject_registered_alias(state_dir, caller)?;
    let named = match ousted_holder.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => {
            validate_identity(name)?;
            Some(name.to_string())
        }
        None => None,
    };
    let now = unix_now();
    if let Some(message) = preview_force_refusal(state_dir, named.as_deref(), now)? {
        return Err(Error::rejected(message));
    }
    // After the holder checks, before any write. The proof peeks the
    // database itself; doing it under the write transaction can stall
    // that peek on the same file.
    require_operator_proof(state_dir, "rollout release --force")?;
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    committed(immediate(&conn, |conn| {
        let Some(lease) = active_lease(conn)? else {
            return Ok(TxResult::Refuse("no rollout lease is held".into()));
        };
        if let Some(message) = force_holder_refusal(&lease, named.as_deref(), now) {
            return Ok(TxResult::Refuse(message));
        }
        mark_ended(conn, lease.id, "released", "force", now)?;
        insert_event(
            conn,
            "rollout_release",
            json!({
                "holder": lease.holder,
                "ousted_holder": lease.holder,
                "lease_id": lease.id,
                "forced": true,
                "by": caller.identity,
                "reason": reason,
            }),
            now,
        )?;
        Ok(TxResult::Done(json!({
            "released": true,
            "forced": true,
            "holder": lease.holder,
            "by": caller.identity,
        })))
    }))
}

/// `--as` must not name a registered pane. The pane's own
/// `$CADENCE_ALIAS` (source `alias`) is still a valid holder.
pub fn reject_registered_alias(state_dir: &Path, caller: &Caller) -> Result<()> {
    if caller.source != "as" {
        return Ok(());
    }
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(());
    }
    let peek = open_peek(&path)?;
    if !table_exists(&peek.conn, "agents")? {
        return Ok(());
    }
    let found: Option<i64> = peek
        .conn
        .query_row(
            "SELECT 1 FROM agents WHERE alias=?1",
            [&caller.identity],
            |row| row.get(0),
        )
        .optional()?;
    if found.is_some() {
        return Err(Error::rejected(format!(
            "--as {} is a registered agent alias; pass an operator identity \
             such as --as operator:name",
            caller.identity
        )));
    }
    Ok(())
}

/// Holder rules for `--force`, shared by the read-only preview and the
/// write so the two cannot drift. `None` means the named holder (or its
/// absence on an expired lease) is acceptable.
fn force_holder_refusal(lease: &Lease, named: Option<&str>, now: f64) -> Option<String> {
    let live = lease.expires_at > now;
    match named.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) if name != lease.holder => Some(format!(
            "--holder {name} does not match the lease holder {}",
            lease.holder
        )),
        None if live => Some(format!(
            "rollout release --force of a live lease held by {} \
             expires_at {} must name that holder with --holder. \
             A live lease is not cleared by --force alone.",
            lease.holder,
            fmt_epoch(lease.expires_at)
        )),
        _ => None,
    }
}

/// The holder decision from a read-only peek, before operator proof and
/// before any write. A missing database is "no lease", not a created file.
fn preview_force_refusal(
    state_dir: &Path,
    named: Option<&str>,
    now: f64,
) -> Result<Option<String>> {
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(Some("no rollout lease is held".into()));
    }
    let peek = open_peek(&path)?;
    match active_lease(&peek.conn)? {
        None => Ok(Some("no rollout lease is held".into())),
        Some(lease) => Ok(force_holder_refusal(&lease, named, now)),
    }
}

/// The operator proof of [`require_operator_proof`], public for verbs
/// outside this module that gate on the same rule (CAD-561's
/// `cadence update`).
pub fn require_operator(state_dir: &Path, verb: &str) -> Result<()> {
    require_operator_proof(state_dir, verb)
}

/// `peer::operator_proof` for this process. Pane pids come from a
/// read-only peek. Enrolled roots come from `slots.json`.
fn require_operator_proof(state_dir: &Path, verb: &str) -> Result<()> {
    // CAD-482: a seam assertion on this process answers before
    // ancestry — a fixture's spawned operator child asserts `operator`
    // in-band, where `/proc` ancestry would refuse a pane's descendant.
    if let Some(who) = crate::test_seam::process_asserted(state_dir)? {
        return match who {
            crate::test_seam::Asserted::Operator => Ok(()),
            crate::test_seam::Asserted::Agent(alias) => Err(Error::rejected(format!(
                "{verb} is an operator action — this process is test-seam \
                 agent '{alias}', not provably the operator"
            ))),
            crate::test_seam::Asserted::Unproven => Err(Error::rejected(format!(
                "{verb} is an operator action — this process carries a \
                 test-seam 'unproven' assertion, not operator proof"
            ))),
        };
    }
    let panes = registered_panes(state_dir)?;
    let roots = enrolled_roots(state_dir)?;
    let daemon_pid = daemon_pid_for_proof(state_dir)?;
    crate::peer::operator_proof(
        std::process::id(),
        unsafe { libc::getuid() },
        daemon_pid,
        &panes,
        |pid| roots.contains(&pid),
    )
    .map_err(|why| {
        Error::rejected(format!(
            "{verb} is an operator action — this process is not \
             provably the operator: {why}; run it from a shell outside every pane \
             and managed endpoint"
        ))
    })
}

/// Registered pty panes (pane pid → alias) for the operator-proof deny
/// list, the same rows [`crate::store::Store::pty_pane_pids`] reads,
/// via the rollout read-only peek, each checked against its recorded
/// start time ([`crate::peer::AgentPids::fenced`], CAD-385): a row
/// whose pid was reused denies nothing; one with no recorded start —
/// including every row of a store older than v14, which has no
/// `pid_start` column — keeps denying. No database, or no `agents`
/// table, means no panes.
fn registered_panes(state_dir: &Path) -> Result<HashMap<u32, String>> {
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let peek = open_peek(&path)?;
    if !table_exists(&peek.conn, "agents")? {
        return Ok(HashMap::new());
    }
    let has_start = peek
        .conn
        .prepare("SELECT 1 FROM pragma_table_info('agents') WHERE name='pid_start'")?
        .exists([])?;
    let start = if has_start { "pid_start" } else { "NULL" };
    let mut stmt = peek.conn.prepare(&format!(
        "SELECT alias, pid, {start} FROM agents \
         WHERE endpoint_kind='pty' AND generation IS NOT NULL AND pid IS NOT NULL"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<i64>>(2)?,
        ))
    })?;
    let mut panes = Vec::new();
    for row in rows {
        let (alias, pid, start) = row?;
        if let Ok(pid) = u32::try_from(pid) {
            panes.push((alias, pid, start.and_then(|s| u64::try_from(s).ok())));
        }
    }
    Ok(crate::peer::AgentPids::classify(panes).fenced())
}

/// Enrollment root pids from `<state>/slots.json`. A missing file means
/// nothing is enrolled. A root whose `/proc` starttime still matches
/// (or cannot be read) counts, including tombstones; a recycled pid
/// does not. Anything the file claims to be but cannot be parsed fails
/// closed — an unreadable enrollment list is not "nothing enrolled".
fn enrolled_roots(state_dir: &Path) -> Result<HashSet<u32>> {
    let path = state_dir.join("slots.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HashSet::new());
        }
        Err(error) => {
            return Err(Error::rejected(format!(
                "rollout release --force cannot read {} ({error})",
                path.display()
            )));
        }
    };
    let doc: Value = serde_json::from_str(&text).map_err(|error| {
        Error::rejected(format!(
            "rollout release --force cannot parse {} ({error})",
            path.display()
        ))
    })?;
    let rows = match doc.get("enrollments") {
        Some(Value::Array(rows)) => rows,
        Some(_) => {
            return Err(Error::rejected(format!(
                "rollout release --force: {} enrollments is not a list",
                path.display()
            )));
        }
        // Legacy v1 holds have no enrollment list.
        None if doc.get("format").is_none() => return Ok(HashSet::new()),
        None => {
            return Err(Error::rejected(format!(
                "rollout release --force: {} has no enrollments to prove against",
                path.display()
            )));
        }
    };
    let mut roots = HashSet::new();
    for row in rows {
        let root = row.get("root").ok_or_else(|| {
            Error::rejected(format!(
                "rollout release --force: {} has an enrollment without root",
                path.display()
            ))
        })?;
        let pid = root
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| {
                Error::rejected(format!(
                    "rollout release --force: {} has an enrollment without root.pid",
                    path.display()
                ))
            })?;
        let starttime = root
            .get("starttime")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                Error::rejected(format!(
                    "rollout release --force: {} has an enrollment without root.starttime",
                    path.display()
                ))
            })?;
        if enrolled_root_matches(pid, starttime) {
            roots.insert(pid);
        }
    }
    Ok(roots)
}

/// Same rule as slot enrollment: an unreadable starttime still matches,
/// so a root we cannot disprove stays enrolled. A different starttime
/// is a recycled pid and is not that root.
fn enrolled_root_matches(pid: u32, starttime: u64) -> bool {
    match proc_starttime(pid) {
        Some(now) => now == starttime,
        None => true,
    }
}

/// Pid of the running daemon, or `0` when it is stopped.
///
/// `serve` holds an exclusive flock on `cadence.lock` for its whole
/// life. The holder recorded in `/proc/locks` is that daemon's pid.
/// When the lock is free, the daemon is stopped and the proof sentinel
/// is `0`: it is not a process, and caller ancestry never contains it.
fn daemon_pid_for_proof(state_dir: &Path) -> Result<u32> {
    Ok(flock_holder(&state_dir.join("cadence.lock"))?.unwrap_or(0))
}

/// `Some(pid)` when an exclusive flock is held. `None` when the file is
/// absent or the lock is free — a successful probe lock is dropped
/// before returning, so this function does not leave a daemon lock behind.
fn flock_holder(path: &Path) -> Result<Option<u32>> {
    flock_holder_from(path, || std::fs::read_to_string("/proc/locks"))
}

/// Reads of `/proc/locks` a held lock gets before its holder counts as
/// missing. The file is not a snapshot: the kernel renders it about a
/// page per `read` and resumes by position, so a lock taken or dropped
/// elsewhere between two reads can skip a live line (CAD-389). A miss
/// re-probes and rereads; only a lock that stays held and unlisted
/// every time is refused.
const PROC_LOCKS_READS: usize = 5;

fn flock_holder_from(
    path: &Path,
    mut read_locks: impl FnMut() -> std::io::Result<String>,
) -> Result<Option<u32>> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;
    let file = match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::rejected(format!(
                "rollout release --force cannot probe {} ({error})",
                path.display()
            )));
        }
    };
    let meta = file.metadata().map_err(|error| {
        Error::rejected(format!(
            "rollout release --force cannot stat {} ({error})",
            path.display()
        ))
    })?;
    let (major, minor) = dev_major_minor(meta.dev());
    let inode = meta.ino();
    for _ in 0..PROC_LOCKS_READS {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(None);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(Error::rejected(format!(
                "rollout release --force cannot probe {} ({error})",
                path.display()
            )));
        }
        let text = read_locks().map_err(|error| {
            Error::rejected(format!(
                "rollout release --force cannot read /proc/locks ({error})"
            ))
        })?;
        if let Some(pid) = proc_locks_flock_writer(&text, major, minor, inode)? {
            return Ok(Some(pid));
        }
    }
    Err(Error::rejected(format!(
        "rollout release --force: {} is locked but its holder is not in /proc/locks",
        path.display()
    )))
}

/// The pid `/proc/locks` lists as the exclusive flock holder of one
/// inode, if the text has that line.
fn proc_locks_flock_writer(text: &str, major: u32, minor: u32, inode: u64) -> Result<Option<u32>> {
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 || fields[1] != "FLOCK" || fields[3] != "WRITE" {
            continue;
        }
        let Some((dev, inode_text)) = fields[5].rsplit_once(':') else {
            continue;
        };
        let Some((major_text, minor_text)) = dev.rsplit_once(':') else {
            continue;
        };
        let (Ok(line_inode), Ok(line_major), Ok(line_minor)) = (
            inode_text.parse::<u64>(),
            u32::from_str_radix(major_text, 16),
            u32::from_str_radix(minor_text, 16),
        ) else {
            continue;
        };
        if line_inode == inode && line_major == major && line_minor == minor {
            let pid = fields[4].parse::<u32>().map_err(|_| {
                Error::rejected("rollout release --force: daemon lock holder pid is not a number")
            })?;
            if pid == 0 {
                return Err(Error::rejected(
                    "rollout release --force: daemon lock holder pid is 0",
                ));
            }
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

/// A device id as `/proc/locks` prints it (`major:minor`).
#[cfg(target_os = "linux")]
fn dev_major_minor(dev: u64) -> (u32, u32) {
    (libc::major(dev), libc::minor(dev))
}

/// Off Linux `dev_t` is 32-bit and there is no `/proc/locks` to match,
/// so `flock_holder` refuses when it cannot read it; this only has to
/// compile to the same shape.
#[cfg(not(target_os = "linux"))]
fn dev_major_minor(dev: u64) -> (u32, u32) {
    let dev = dev as libc::dev_t;
    (libc::major(dev) as u32, libc::minor(dev) as u32)
}

pub fn handoff(state_dir: &Path, caller: &Caller, to: &str) -> Result<Value> {
    validate_identity(to)?;
    if to == caller.identity {
        return Err(Error::rejected(
            "handoff --to must name a different identity",
        ));
    }
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    committed(immediate(&conn, |conn| {
        let lease = match require_holder(conn, caller, now, false)? {
            TxResult::Done(lease) => lease,
            TxResult::Refuse(message) => return Ok(TxResult::Refuse(message)),
        };
        mark_ended(conn, lease.id, "handed_off", "handoff", now)?;
        conn.execute(
            "INSERT INTO rollout_leases(
                holder, holder_source, host_note, reason, target_commit,
                schema_from, schema_to, backup_path, backup_sha256, backup_schema,
                backup_taken_at, claimed_at, expires_at, status, previous_holder)
             VALUES(?1,'handoff',?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'active',?13)",
            params![
                to,
                host_note(),
                lease.reason,
                lease.target_commit,
                lease.schema_from,
                lease.schema_to,
                lease.backup_path,
                lease.backup_sha256,
                lease.backup_schema,
                lease.backup_taken_at,
                lease.claimed_at,
                lease.expires_at,
                lease.holder,
            ],
        )?;
        insert_event(
            conn,
            "rollout_handoff",
            json!({"from": lease.holder, "to": to, "lease_id": lease.id}),
            now,
        )?;
        Ok(TxResult::Done(
            json!({"handed_off": true, "from": lease.holder, "to": to}),
        ))
    }))
}

pub fn record_backup(state_dir: &Path, caller: &Caller, backup: &Path) -> Result<Value> {
    let canon = canonical_backup(state_dir, backup)?;
    let (sha, schema) = inspect_backup(&canon)?;
    let mtime = file_mtime(&canon)?;
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    committed(immediate(&conn, |conn| {
        let lease = match require_holder(conn, caller, now, false)? {
            TxResult::Done(lease) => lease,
            TxResult::Refuse(message) => return Ok(TxResult::Refuse(message)),
        };
        // Cheap early refusal. The lease-row check below is the
        // provenance proof: `touch` and `cp` refresh mtime without
        // copying this claim's row.
        if mtime + 1.0 < lease.claimed_at {
            return Ok(TxResult::Refuse(format!(
                "backup {} was last modified before this lease was claimed at {}; \
                 take a new copy after `cadence rollout claim`",
                canon.display(),
                fmt_epoch(lease.claimed_at)
            )));
        }
        if !backup_contains_lease(&canon, &lease)? {
            return Ok(TxResult::Refuse(format!(
                "backup {} does not contain this lease (id {}, holder {}, claimed_at {}). \
                 Take the copy after `cadence rollout claim`. A file from before the claim, \
                 a touched old copy, or a foreign database is not provenance.",
                canon.display(),
                lease.id,
                lease.holder,
                fmt_epoch(lease.claimed_at)
            )));
        }
        conn.execute(
            "UPDATE rollout_leases
             SET backup_path=?1, backup_sha256=?2, backup_schema=?3, backup_taken_at=?4
             WHERE id=?5",
            params![canon.display().to_string(), sha, schema, now, lease.id],
        )?;
        insert_event(
            conn,
            "rollout_backup_recorded",
            json!({
                "holder": lease.holder,
                "path": canon,
                "sha256": sha,
                "schema_version": schema,
                "taken_at": now,
            }),
            now,
        )?;
        Ok(TxResult::Done(json!({
            "recorded": true,
            "holder": lease.holder,
            "path": canon,
            "sha256": sha,
            "schema_version": schema,
            "taken_at": now,
        })))
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateIngest {
    pub inserted: usize,
    pub skipped: usize,
}

pub fn ingest_gate_log(state_dir: &Path, conn: &Connection) -> Result<GateIngest> {
    let path = state_dir.join(GATE_LOG);
    if !path.exists() {
        return Ok(GateIngest {
            inserted: 0,
            skipped: 0,
        });
    }
    let text = std::fs::read_to_string(&path)?;
    // Drop the file before inserting. A failure halfway through must
    // not replay the lines that already landed.
    std::fs::remove_file(&path)?;
    let mut inserted = 0;
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match gate_line(line) {
            Some((kind, payload)) => {
                insert_event(conn, kind, payload, unix_now())?;
                inserted += 1;
            }
            None => skipped += 1,
        }
    }
    Ok(GateIngest { inserted, skipped })
}

/// Shapes `append_gate_log` writes. The line's `at` is ignored; ingest
/// stamps the time and `source` itself. Anything else is skipped, so a
/// writer who can drop a line in the state dir still cannot insert an
/// arbitrary event.
fn gate_line(line: &str) -> Option<(&'static str, Value)> {
    let value: Value = serde_json::from_str(line).ok()?;
    let kind = value.get("kind")?.as_str()?;
    let payload = value.get("payload")?.as_object()?;
    match kind {
        "rollout_migration_refused" => {
            let from = payload.get("from")?.as_i64()?;
            let to = payload.get("to")?.as_i64()?;
            let reason = payload.get("reason")?.as_str()?;
            if reason.is_empty() {
                return None;
            }
            Some((
                "rollout_migration_refused",
                json!({
                    "from": from,
                    "to": to,
                    "reason": reason,
                    "source": "gate_log",
                }),
            ))
        }
        "rollout_start_refused" => {
            let build = nonempty(payload.get("build")?.as_str()?)?;
            let recorded = payload.get("recorded")?.as_str()?;
            let reason = nonempty(payload.get("reason")?.as_str()?)?;
            let holder = payload.get("holder").and_then(Value::as_str).unwrap_or("");
            Some((
                "rollout_start_refused",
                json!({
                    "build": build,
                    "recorded": recorded,
                    "holder": holder,
                    "reason": reason,
                    "source": "gate_log",
                }),
            ))
        }
        _ => None,
    }
}

fn nonempty(text: &str) -> Option<&str> {
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn claim_in(conn: &Connection, req: &ClaimRequest<'_>) -> Result<TxResult<Value>> {
    // CAD-384: an agent (the identity is its pane's `CADENCE_ALIAS`)
    // claims only under a live operator grant — the lease is what lets a
    // pane stop the production daemon.
    if req.caller.source == "alias" && live_grant(conn, &req.caller.identity, req.now)?.is_none() {
        let message = format!(
            "refusing claim: agent '{}' holds no rollout grant — the operator grants \
             one from a shell outside every pane (`cadence rollout grant {}`), or \
             claims the lease itself with `--as operator:<name>`",
            req.caller.identity, req.caller.identity
        );
        insert_event(
            conn,
            "rollout_claim_refused",
            json!({"holder": req.caller.identity, "reason": message}),
            req.now,
        )?;
        return Ok(TxResult::Refuse(message));
    }
    let existing = active_lease(conn)?;
    if let Some(lease) = existing {
        if lease.expires_at <= req.now {
            if !req.takeover {
                insert_event(
                    conn,
                    "rollout_expiry",
                    json!({"holder": lease.holder, "expires_at": lease.expires_at}),
                    req.now,
                )?;
                let message = format!(
                    "refusing claim: rollout lease held by {} target {} expired at {}; \
                     pass --takeover to claim it",
                    lease.holder,
                    target_of(&lease),
                    fmt_epoch(lease.expires_at)
                );
                insert_event(
                    conn,
                    "rollout_claim_refused",
                    json!({"holder": req.caller.identity, "reason": message}),
                    req.now,
                )?;
                return Ok(TxResult::Refuse(message));
            }
            mark_ended(conn, lease.id, "taken_over", "takeover", req.now)?;
            insert_event(
                conn,
                "rollout_expiry",
                json!({"holder": lease.holder, "expires_at": lease.expires_at}),
                req.now,
            )?;
            let created = insert_lease(conn, req, Some(&lease.holder))?;
            insert_event(
                conn,
                "rollout_takeover",
                json!({
                    "holder": req.caller.identity,
                    "previous_holder": lease.holder,
                    "target": req.target,
                    "expires_at": created.expires_at,
                }),
                req.now,
            )?;
            return Ok(TxResult::Done(held_status(&created, req.now)));
        }
        let message = format!(
            "refusing claim: rollout lease held by {} target {} expires_at {}",
            lease.holder,
            target_of(&lease),
            fmt_epoch(lease.expires_at)
        );
        if req.takeover {
            let message = format!("{message}; --takeover is only allowed on an expired lease");
            insert_event(
                conn,
                "rollout_claim_refused",
                json!({"holder": req.caller.identity, "reason": message, "held_by": lease.holder}),
                req.now,
            )?;
            return Ok(TxResult::Refuse(message));
        }
        insert_event(
            conn,
            "rollout_claim_refused",
            json!({
                "holder": req.caller.identity,
                "reason": message,
                "held_by": lease.holder,
                "target": lease.target_commit,
                "expires_at": lease.expires_at,
            }),
            req.now,
        )?;
        return Ok(TxResult::Refuse(message));
    }
    if req.takeover {
        return Ok(TxResult::Refuse(
            "refusing --takeover: no rollout lease is held".into(),
        ));
    }
    let created = insert_lease(conn, req, None)?;
    insert_event(
        conn,
        "rollout_claim",
        json!({
            "holder": created.holder,
            "holder_source": created.holder_source,
            "target": created.target_commit,
            "reason": created.reason,
            "expires_at": created.expires_at,
            "schema_from": created.schema_from,
            "schema_to": created.schema_to,
        }),
        req.now,
    )?;
    Ok(TxResult::Done(held_status(&created, req.now)))
}

fn insert_lease(
    conn: &Connection,
    req: &ClaimRequest<'_>,
    previous: Option<&str>,
) -> Result<Lease> {
    let schema_from = read_user_schema(conn)?;
    let expires_at = req.now + req.ttl.as_secs_f64();
    conn.execute(
        "INSERT INTO rollout_leases(
            holder, holder_source, host_note, reason, target_commit,
            schema_from, schema_to, claimed_at, expires_at, status, previous_holder)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'active',?10)",
        params![
            req.caller.identity,
            req.caller.source,
            host_note(),
            req.reason,
            req.target,
            schema_from,
            SCHEMA_VERSION,
            req.now,
            expires_at,
            previous,
        ],
    )?;
    let id = conn.last_insert_rowid();
    lease_by_id(conn, id)?.ok_or_else(|| Error::internal("lease insert did not persist"))
}

fn require_holder(
    conn: &Connection,
    caller: &Caller,
    now: f64,
    allow_expired: bool,
) -> Result<TxResult<Lease>> {
    let Some(lease) = active_lease(conn)? else {
        return Ok(TxResult::Refuse("no rollout lease is held".into()));
    };
    if lease.holder != caller.identity {
        return Ok(TxResult::Refuse(with_expired_hint(
            &lease,
            now,
            format!(
                "rollout lease is held by {} target {} expires_at {}; only the holder can do this.",
                lease.holder,
                target_of(&lease),
                fmt_epoch(lease.expires_at)
            ),
        )));
    }
    if lease.expires_at <= now && !allow_expired {
        return Ok(TxResult::Refuse(with_expired_hint(
            &lease,
            now,
            format!(
                "rollout lease held by {} expired at {}; pass --takeover to claim it.",
                lease.holder,
                fmt_epoch(lease.expires_at)
            ),
        )));
    }
    Ok(TxResult::Done(lease))
}

/// `None` when `caller` holds an unexpired lease. Otherwise the refusal text,
/// which names the holder, target, and expiry when another lease is active.
fn holder_block(conn: &Connection, caller: &Caller, now: f64) -> Result<Option<String>> {
    let Some(lease) = active_lease(conn)? else {
        return Ok(Some(
            "no rollout lease is held. Run `cadence rollout claim --reason \"<why>\" --as <identity>`"
                .into(),
        ));
    };
    if lease.expires_at <= now {
        return Ok(Some(with_expired_hint(
            &lease,
            now,
            format!(
                "rollout lease held by {} target {} expired at {}; pass --takeover to claim it",
                lease.holder,
                target_of(&lease),
                fmt_epoch(lease.expires_at)
            ),
        )));
    }
    if lease.holder != caller.identity {
        return Ok(Some(with_expired_hint(
            &lease,
            now,
            format!(
                "rollout lease held by {} target {} expires_at {}; only the holder can do this.",
                lease.holder,
                target_of(&lease),
                fmt_epoch(lease.expires_at)
            ),
        )));
    }
    Ok(None)
}

/// The force-release hint is how a dead holder is cleared. Showing it
/// on a healthy lease tells every refused caller how to oust the
/// legitimate holder, which is the failure the lease exists to prevent.
fn with_expired_hint(lease: &Lease, now: f64, body: String) -> String {
    if lease.expires_at <= now {
        format!("{body} {FORCE_RELEASE_HINT}")
    } else {
        body
    }
}

fn qualifying_receipt(path: &Path, version: i64, holder: Option<&str>) -> Result<bool> {
    let Some(holder) = holder.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    if !lease_table_present(path)? {
        return Ok(false);
    }
    let peek = open_peek(path)?;
    let Some(lease) = active_lease(&peek.conn)? else {
        return Ok(false);
    };
    if lease.holder != holder || lease.expires_at <= unix_now() {
        return Ok(false);
    }
    Ok(lease.backup_sha256.is_some()
        && lease.backup_schema == Some(version)
        && lease.backup_taken_at.unwrap_or(0.0) >= lease.claimed_at)
}

fn held_status(lease: &Lease, now: f64) -> Value {
    json!({
        "held": true,
        "holder": lease.holder,
        "holder_source": lease.holder_source,
        "host_note": lease.host_note,
        "reason": lease.reason,
        "target": lease.target_commit,
        "schema_from": lease.schema_from,
        "schema_to": lease.schema_to,
        "backup": backup_json(lease),
        "claimed_at": lease.claimed_at,
        "expires_at": lease.expires_at,
        "age_secs": now - lease.claimed_at,
        "expires_in_secs": lease.expires_at - now,
        "previous_holder": lease.previous_holder,
    })
}

fn backup_json(lease: &Lease) -> Value {
    match &lease.backup_sha256 {
        Some(sha) => json!({
            "path": lease.backup_path,
            "sha256": sha,
            "schema_version": lease.backup_schema,
            "taken_at": lease.backup_taken_at,
        }),
        None => Value::Null,
    }
}

fn target_of(lease: &Lease) -> String {
    lease.target_commit.clone().unwrap_or_else(|| "none".into())
}

fn fmt_epoch(epoch: f64) -> String {
    format!("{epoch:.3}")
}

fn validate_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() || reason.len() > 500 || reason.chars().any(|c| c.is_control()) {
        return Err(Error::rejected(
            "--reason must be 1..=500 characters with no control characters",
        ));
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<()> {
    let ok = (7..=64).contains(&target.len()) && target.chars().all(|c| c.is_ascii_hexdigit());
    if !ok {
        return Err(Error::rejected("--target must be 7..=64 hex characters"));
    }
    Ok(())
}

fn host_note() -> String {
    let host =
        std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_else(|_| "unknown".into());
    format!("host={} pid={}", host.trim(), std::process::id())
}

fn canonical_backup(state_dir: &Path, backup: &Path) -> Result<PathBuf> {
    let canon = std::fs::canonicalize(backup).map_err(|error| {
        Error::rejected(format!(
            "backup path {} is not a readable file: {error}",
            backup.display()
        ))
    })?;
    let live = db_file(state_dir);
    if let Ok(live_canon) = std::fs::canonicalize(&live) {
        let wal = sidecar(&live_canon, "-wal");
        let shm = sidecar(&live_canon, "-shm");
        if canon == live_canon || canon == wal || canon == shm {
            return Err(Error::rejected(format!(
                "refusing to record the live database {} as its own backup. \
                 Copy it somewhere outside the state dir, then pass that copy \
                 to `cadence rollout backup --path`",
                canon.display()
            )));
        }
    }
    if let Ok(state) = std::fs::canonicalize(state_dir) {
        if canon.starts_with(&state) {
            return Err(Error::rejected(format!(
                "refusing backup {}: it is inside the state dir. Copy the \
                 database somewhere else, then pass that copy to \
                 `cadence rollout backup --path`",
                canon.display()
            )));
        }
    }
    // `canonicalize` does not collapse hard links or bind mounts. The
    // same device and inode is the same file, whichever path names it.
    let wal = sidecar(&live, "-wal");
    let shm = sidecar(&live, "-shm");
    if same_file(&canon, &live) || same_file(&canon, &wal) || same_file(&canon, &shm) {
        return Err(Error::rejected(format!(
            "refusing to record {} as a backup: it is the same file as the \
             live database {} (hard link or bind mount). Copy it somewhere \
             outside the state dir, then pass that copy to \
             `cadence rollout backup --path`",
            canon.display(),
            live.display()
        )));
    }
    Ok(canon)
}

fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(left) = std::fs::metadata(a) else {
        return false;
    };
    let Ok(right) = std::fs::metadata(b) else {
        return false;
    };
    left.dev() == right.dev() && left.ino() == right.ino()
}

/// The backup proves provenance only when it holds this claim's row.
/// `claim` writes that row before any backup, so a copy taken earlier,
/// a touched old file, or a foreign database does not have it.
fn backup_contains_lease(backup: &Path, lease: &Lease) -> Result<bool> {
    let peek = open_peek(backup)?;
    if !table_exists(&peek.conn, "rollout_leases")? {
        return Ok(false);
    }
    let found: Option<i64> = peek
        .conn
        .query_row(
            "SELECT 1 FROM rollout_leases WHERE id=?1 AND holder=?2 AND claimed_at=?3",
            params![lease.id, lease.holder, lease.claimed_at],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn file_mtime(path: &Path) -> Result<f64> {
    let modified = std::fs::metadata(path)
        .map_err(|error| {
            Error::rejected(format!(
                "backup path {} is not readable: {error}",
                path.display()
            ))
        })?
        .modified()
        .map_err(|error| {
            Error::rejected(format!(
                "backup path {} has no mtime: {error}",
                path.display()
            ))
        })?;
    Ok(modified
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0))
}

fn inspect_backup(path: &Path) -> Result<(String, i64)> {
    if !path.is_file() {
        return Err(Error::rejected(format!(
            "backup path {} is not a readable file",
            path.display()
        )));
    }
    let mut file = File::open(path).map_err(|e| {
        Error::rejected(format!(
            "backup path {} is not readable: {e}",
            path.display()
        ))
    })?;
    let mut header = [0u8; 16];
    let n = file.read(&mut header).unwrap_or(0);
    if n < 16 || &header != b"SQLite format 3\0" {
        return Err(Error::rejected(format!(
            "backup path {} is not a SQLite database",
            path.display()
        )));
    }
    let mut hasher = Sha256::new();
    hasher.update(header);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| Error::internal(format!("reading backup: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let sha = hex_encode(&hasher.finalize());
    let schema = peek_user_schema(path)?.ok_or_else(|| {
        Error::rejected(format!(
            "backup {} has no cadence schema_version",
            path.display()
        ))
    })?;
    Ok((sha, schema))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn connect(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(store::BUSY_TIMEOUT)?;
    Ok(conn)
}

fn connect_ensured(path: &Path) -> Result<Connection> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = connect(path)?;
    ensure_lease_tables(&conn)?;
    Ok(conn)
}

fn immediate<T>(conn: &Connection, body: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match body(conn) {
        Ok(value) => {
            conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn active_lease(conn: &Connection) -> Result<Option<Lease>> {
    if !table_exists(conn, "rollout_leases")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT id, holder, holder_source, host_note, reason, target_commit,
                schema_from, schema_to, backup_path, backup_sha256, backup_schema,
                backup_taken_at, claimed_at, expires_at, status, previous_holder
         FROM rollout_leases WHERE status='active' LIMIT 1",
        [],
        row_lease,
    )
    .optional()
    .map_err(Into::into)
}

fn lease_by_id(conn: &Connection, id: i64) -> Result<Option<Lease>> {
    conn.query_row(
        "SELECT id, holder, holder_source, host_note, reason, target_commit,
                schema_from, schema_to, backup_path, backup_sha256, backup_schema,
                backup_taken_at, claimed_at, expires_at, status, previous_holder
         FROM rollout_leases WHERE id=?1",
        [id],
        row_lease,
    )
    .optional()
    .map_err(Into::into)
}

fn row_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    Ok(Lease {
        id: row.get(0)?,
        holder: row.get(1)?,
        holder_source: row.get(2)?,
        host_note: row.get(3)?,
        reason: row.get(4)?,
        target_commit: row.get(5)?,
        schema_from: row.get(6)?,
        schema_to: row.get(7)?,
        backup_path: row.get(8)?,
        backup_sha256: row.get(9)?,
        backup_schema: row.get(10)?,
        backup_taken_at: row.get(11)?,
        claimed_at: row.get(12)?,
        expires_at: row.get(13)?,
        status: row.get(14)?,
        previous_holder: row.get(15)?,
    })
}

fn mark_ended(conn: &Connection, id: i64, status: &str, reason: &str, at: f64) -> Result<()> {
    conn.execute(
        "UPDATE rollout_leases SET status=?1, end_reason=?2, ended_at=?3 WHERE id=?4",
        params![status, reason, at, id],
    )?;
    Ok(())
}

fn read_build(conn: &Connection) -> Result<Option<String>> {
    if !table_exists(conn, "daemon_build")? {
        return Ok(None);
    }
    conn.query_row("SELECT commit_sha FROM daemon_build WHERE id=1", [], |r| {
        r.get(0)
    })
    .optional()
    .map_err(Into::into)
}

fn read_user_schema(conn: &Connection) -> Result<Option<i64>> {
    if !table_exists(conn, "schema_version")? {
        return Ok(None);
    }
    let version: i64 = conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
    Ok(Some(version))
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn insert_event(conn: &Connection, kind: &str, payload: Value, at: f64) -> Result<()> {
    if !table_exists(conn, "events")? {
        return Ok(());
    }
    let cols = column_names(conn, "events")?;
    if cols.iter().any(|c| c == "job_id") {
        conn.execute(
            "INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?1,?2,?3,NULL,NULL,?4)",
            params![Store::DAEMON_STREAM, kind, payload.to_string(), at],
        )?;
    } else {
        conn.execute(
            "INSERT INTO events(alias,kind,payload,at) VALUES(?1,?2,?3,?4)",
            params![Store::DAEMON_STREAM, kind, payload.to_string(), at],
        )?;
    }
    Ok(())
}

fn column_names(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

enum Peek {
    Missing,
    Fresh,
    Version(i64),
}

struct PeekConn {
    _tmp: Option<tempfile::TempDir>,
    conn: Connection,
}

fn peek_schema(path: &Path) -> Result<Peek> {
    if !path.exists() {
        return Ok(Peek::Missing);
    }
    match peek_user_schema(path)? {
        None | Some(0) => Ok(Peek::Fresh),
        Some(version) => Ok(Peek::Version(version)),
    }
}

fn peek_user_schema(path: &Path) -> Result<Option<i64>> {
    let peek = open_peek(path)?;
    if !table_exists(&peek.conn, "schema_version")? {
        return Ok(None);
    }
    let version: i64 = peek
        .conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
    Ok(Some(version))
}

fn lease_table_present(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let peek = open_peek(path)?;
    table_exists(&peek.conn, "rollout_leases")
}

/// Read the database without writing it. A non-empty `-wal` is copied
/// with its sidecars so a lease that exists only in the WAL is visible;
/// `immutable=1` would ignore that WAL.
fn open_peek(path: &Path) -> Result<PeekConn> {
    if wal_nonempty(path) {
        return open_peek_copy(path);
    }
    match open_immutable(path) {
        Ok(conn) => Ok(PeekConn { _tmp: None, conn }),
        Err(_) => open_peek_copy(path),
    }
}

fn wal_nonempty(path: &Path) -> bool {
    std::fs::metadata(sidecar(path, "-wal"))
        .map(|meta| meta.len() > 32)
        .unwrap_or(false)
}

fn open_peek_copy(path: &Path) -> Result<PeekConn> {
    let tmp =
        tempfile::tempdir().map_err(|error| Error::internal(format!("peek copy: {error}")))?;
    let copy = tmp.path().join("peek.sqlite3");
    std::fs::copy(path, &copy)?;
    for suffix in ["-wal", "-shm"] {
        let side = sidecar(path, suffix);
        if side.exists() {
            std::fs::copy(&side, sidecar(&copy, suffix))?;
        }
    }
    let conn = Connection::open(&copy)?;
    Ok(PeekConn {
        _tmp: Some(tmp),
        conn,
    })
}

fn open_immutable(path: &Path) -> Result<Connection> {
    let uri = format!("file:{}?mode=ro&immutable=1", encode_uri_path(path));
    Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(Into::into)
}

fn encode_uri_path(path: &Path) -> String {
    let mut out = String::new();
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn append_gate_log(db: &Path, kind: &str, payload: Value) -> Result<()> {
    let Some(dir) = db.parent() else {
        return Ok(());
    };
    let line = json!({"kind": kind, "payload": payload, "at": unix_now()}).to_string();
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(GATE_LOG))?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn caller(name: &str) -> Caller {
        Caller {
            identity: name.into(),
            source: "as",
        }
    }

    fn fresh(dir: &tempfile::TempDir) -> PathBuf {
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let db = db_file(&state);
        Store::open(&db).unwrap();
        state
    }

    fn claim_as(
        state: &Path,
        name: &str,
        now: f64,
        ttl: Duration,
        takeover: bool,
    ) -> Result<Value> {
        let c = caller(name);
        claim(
            state,
            &ClaimRequest {
                caller: &c,
                reason: "test",
                target: Some("abcdef1"),
                ttl,
                takeover,
                now,
            },
        )
    }

    fn checkpoint(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(conn);
        let _ = std::fs::remove_file(sidecar(path, "-wal"));
        let _ = std::fs::remove_file(sidecar(path, "-shm"));
    }

    fn downgrade_to_v11(state: &Path) -> PathBuf {
        let db = db_file(state);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE IF EXISTS rollout_leases;
             DROP TABLE IF EXISTS daemon_build;
             UPDATE schema_version SET version=11;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
        drop(conn);
        let _ = std::fs::remove_file(sidecar(&db, "-wal"));
        let _ = std::fs::remove_file(sidecar(&db, "-shm"));
        db
    }

    #[test]
    fn identity_comes_from_alias_or_as_never_from_a_path() {
        let err = resolve_caller_with(None, None).unwrap_err();
        assert!(err.to_string().contains("--as"), "{err}");
        let c = resolve_caller_with(None, Some("operator:ada")).unwrap();
        assert_eq!(c.identity, "operator:ada");
        assert_eq!(c.source, "as");
        let c = resolve_caller_with(Some("pane"), None).unwrap();
        assert_eq!((c.identity.as_str(), c.source), ("pane", "alias"));
        let err = resolve_caller_with(Some("pane"), Some("operator:ada")).unwrap_err();
        assert!(err.to_string().contains("CADENCE_ALIAS"), "{err}");
        let err = resolve_caller_with(None, Some("/tmp/worktree")).unwrap();
        assert_eq!(err.identity, "/tmp/worktree");
    }

    #[test]
    fn second_claim_names_holder_target_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let ok = claim_as(&state, "alice", 1_000.0, Duration::from_secs(60), false).unwrap();
        assert_eq!(ok["held"], true);
        assert_eq!(ok["holder"], "alice");
        assert_eq!(ok["target"], "abcdef1");
        let err = claim_as(&state, "bob", 1_010.0, Duration::from_secs(60), false).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains("abcdef1"), "{text}");
        assert!(text.contains("1060.000"), "{text}");
        let events = events_of(&state);
        assert!(events.iter().any(|e| e.0 == "rollout_claim"));
        assert!(events.iter().any(|e| e.0 == "rollout_claim_refused"));
    }

    #[test]
    fn release_and_handoff_are_holder_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let err = release(&state, &caller("bob")).unwrap_err();
        assert!(err.to_string().contains("only the holder"), "{err}");
        let err = handoff(&state, &caller("bob"), "carol").unwrap_err();
        assert!(err.to_string().contains("only the holder"), "{err}");
        handoff(&state, &caller("alice"), "carol").unwrap();
        let status = status_at(&state);
        assert_eq!(status["holder"], "carol");
        assert_eq!(status["previous_holder"], "alice");
        release(&state, &caller("carol")).unwrap();
        let status = super::status(&state).unwrap();
        assert_eq!(status["held"], false);
        let events = events_of(&state);
        assert!(events.iter().any(|e| e.0 == "rollout_handoff"));
        assert!(events.iter().any(|e| e.0 == "rollout_release"));
    }

    #[test]
    fn renew_extends_only_the_holders_own_live_lease() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        // CAD-482: the claim is an operator action; assert it in-band so
        // this runs identically in a pane and in CI.
        let seam = state.join("seam");
        std::fs::create_dir_all(&seam).unwrap();
        std::fs::write(seam.join("token"), "test-token").unwrap();
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            claim_as(&state, "alice", 1_000.0, Duration::from_secs(60), false).unwrap();
            // Another identity cannot renew it.
            let err = renew(
                &state,
                &caller("bob"),
                "bob's work",
                Some("fffffff"),
                Duration::from_secs(3600),
                1_010.0,
            )
            .unwrap_err();
            assert!(err.to_string().contains("only the holder"), "{err}");
            // The holder's renewal moves the expiry in place: the same
            // row, the same claimed_at, no release and re-claim. The
            // renewing run's reason and target land on the row too, so
            // `rollout status` reports what is actually running
            // (CAD-561 r4).
            let before = status_at(&state);
            let renewed = renew(
                &state,
                &caller("alice"),
                "cadence update",
                Some("1234abc"),
                Duration::from_secs(3600),
                1_010.0,
            )
            .unwrap();
            assert_eq!(renewed["renewed"], true);
            assert_eq!(renewed["expires_at"].as_f64().unwrap(), 1_010.0 + 3600.0);
            let after = status_at(&state);
            assert_eq!(after["claimed_at"], before["claimed_at"]);
            assert_eq!(after["expires_at"].as_f64().unwrap(), 1_010.0 + 3600.0);
            assert_eq!(after["reason"], "cadence update");
            assert_eq!(after["target"], "1234abc");
            // An expired lease is not renewable — the holder takes over
            // or claims afresh.
            let err = renew(
                &state,
                &caller("alice"),
                "cadence update",
                Some("1234abc"),
                Duration::from_secs(60),
                9_999.0,
            )
            .unwrap_err();
            assert!(err.to_string().contains("--takeover"), "{err}");
        });
        let events = events_of(&state);
        assert!(events.iter().any(|e| e.0 == "rollout_renew"));
    }

    #[test]
    fn expired_claim_refusal_still_allows_takeover() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(&state, "alice", 1_000.0, Duration::from_secs(10), false).unwrap();
        let err = claim_as(&state, "bob", 1_020.0, Duration::from_secs(30), false).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains("--takeover"), "{text}");
        let view = super::status(&state).unwrap();
        assert_eq!(view["expired"], true, "{view}");
        assert_eq!(view["holder"], "alice");
        let err = release(&state, &caller("alice")).unwrap_err();
        assert!(err.to_string().contains("--takeover"), "{err}");
        let ok = claim_as(&state, "bob", 1_020.0, Duration::from_secs(30), true).unwrap();
        assert_eq!(ok["holder"], "bob");
        assert_eq!(ok["previous_holder"], "alice");
        let events = events_of(&state);
        let takeover = events
            .iter()
            .find(|e| e.0 == "rollout_takeover")
            .map(|e| &e.1)
            .unwrap();
        assert!(takeover.contains("alice"), "{takeover}");
        assert!(takeover.contains("previous_holder"), "{takeover}");
    }

    #[test]
    fn parallel_claims_have_one_winner_and_the_loser_names_them() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for name in ["alice", "bob"] {
            let state = state.clone();
            let barrier = Arc::clone(&barrier);
            let name = name.to_string();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                claim_as(&state, &name, 5_000.0, Duration::from_secs(60), false)
            }));
        }
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let wins = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "{results:?}");
        let winner = results.iter().find_map(|r| r.as_ref().ok()).unwrap()["holder"]
            .as_str()
            .unwrap()
            .to_string();
        let loser = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert!(
            loser.to_string().contains(&winner),
            "{loser} winner {winner}"
        );
    }

    #[test]
    fn backup_records_sha_and_schema_for_the_holder_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let db = db_file(&state);
        checkpoint(&db);
        let backup = dir.path().join("backup.sqlite3");
        std::fs::copy(&db, &backup).unwrap();
        let err = record_backup(&state, &caller("bob"), &backup).unwrap_err();
        assert!(err.to_string().contains("alice"), "{err}");
        let bad = dir.path().join("not-sqlite");
        std::fs::write(&bad, b"hello").unwrap();
        let err = record_backup(&state, &caller("alice"), &bad).unwrap_err();
        assert!(err.to_string().contains("not a SQLite"), "{err}");
        let recorded = record_backup(&state, &caller("alice"), &backup).unwrap();
        assert_eq!(recorded["schema_version"], SCHEMA_VERSION);
        assert_eq!(recorded["sha256"].as_str().unwrap().len(), 64);
        let events = events_of(&state);
        assert!(events.iter().any(|e| e.0 == "rollout_backup_recorded"));
    }

    #[test]
    fn lower_schema_without_a_receipt_is_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        checkpoint(&db);
        let before = std::fs::read(&db).unwrap();
        let err = authorize_migration(&db).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("cadence rollout claim"), "{text}");
        assert!(text.contains("cadence rollout backup"), "{text}");
        assert!(text.contains("not modified"), "{text}");
        assert!(text.contains("CADENCE_ROLLOUT_BOOTSTRAP"), "{text}");
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert!(!sidecar(&db, "-wal").exists());
        let log = std::fs::read_to_string(state.join(GATE_LOG)).unwrap();
        assert!(log.contains("rollout_migration_refused"), "{log}");
    }

    #[test]
    fn matching_receipt_allows_the_migration_and_a_fresh_dir_does_not_need_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(authorize_migration(&db_file(&dir.path().join("missing")))
            .unwrap()
            .crossing
            .is_none());
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        checkpoint(&db);
        let backup = dir.path().join("backup.sqlite3");
        std::fs::copy(&db, &backup).unwrap();
        record_backup(&state, &caller("alice"), &backup).unwrap();
        let _holder = hold_migration("alice");
        let permit = authorize_migration(&db).unwrap();
        assert_eq!(permit.crossing.unwrap().reason, "backup_receipt");
        let store = Store::open(&db).unwrap();
        drop(store);
        let version: i64 = Connection::open(&db)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn bootstrap_allows_only_the_lease_table_introduction() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        BOOTSTRAP_FORCE.with(|c| c.set(Some(true)));
        let permit = authorize_migration(&db);
        BOOTSTRAP_FORCE.with(|c| c.set(None));
        assert_eq!(
            permit.unwrap().crossing.unwrap().reason,
            "lease_table_introduction"
        );

        let state = dir.path().join("state-b");
        std::fs::create_dir_all(&state).unwrap();
        Store::open(&db_file(&state)).unwrap();
        let db = downgrade_to_v11(&state);
        claim_as(&state, "alice", unix_now(), Duration::from_secs(60), false).unwrap();
        BOOTSTRAP_FORCE.with(|c| c.set(Some(true)));
        let err = authorize_migration(&db);
        BOOTSTRAP_FORCE.with(|c| c.set(None));
        let err = err.unwrap_err();
        assert!(err.to_string().contains("backup"), "{err}");
    }

    /// CAD-310: a sandbox's own state dir starts a different build and
    /// crosses a schema with no identity and no lease; the same dir
    /// without its marker still refuses both.
    #[test]
    fn a_marked_sandbox_state_dir_needs_no_lease_for_a_new_build_or_schema() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sbx");
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        Store::open(&db_file(&state)).unwrap();
        let conn = connect_ensured(&db_file(&state)).unwrap();
        upsert_daemon_build(&conn, "deadbeef", 1.0).unwrap();
        drop(conn);
        let nobody = || Err(Error::rejected("no rollout identity"));
        assert!(authorize_spawn_for(&state, nobody()).is_err());
        std::fs::write(root.join(".cadence-sandbox"), r#"{"name":"sbx"}"#).unwrap();
        assert!(sandbox_exempt(&state));
        assert!(authorize_spawn_for(&state, nobody()).is_ok());
        let db = downgrade_to_v11(&state);
        assert!(authorize_migration(&db).is_ok());
        std::fs::remove_file(root.join(".cadence-sandbox")).unwrap();
        assert!(authorize_migration(&db).is_err());
    }

    #[test]
    fn same_build_starts_without_a_lease_and_a_different_build_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let conn = connect_ensured(&db_file(&state)).unwrap();
        assert!(authorize_spawn_for(&state, resolve_caller_with(None, None)).is_ok());
        upsert_daemon_build(&conn, crate::overview::BUILD_COMMIT, 1.0).unwrap();
        assert!(authorize_spawn_for(&state, resolve_caller_with(None, None)).is_ok());
        upsert_daemon_build(&conn, "deadbeef", 1.0).unwrap();
        let err = authorize_spawn_for(&state, Ok(caller("operator:ada"))).unwrap_err();
        assert!(err.to_string().contains("deadbeef"), "{err}");
        claim_as(
            &state,
            "operator:ada",
            unix_now(),
            Duration::from_secs(60),
            false,
        )
        .unwrap();
        assert!(authorize_spawn_for(&state, Ok(caller("operator:ada"))).is_ok());
    }

    #[test]
    fn restart_recheck_reports_release_handoff_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let ticket = begin_restart(&state, &caller("alice")).unwrap();
        release(&state, &caller("alice")).unwrap();
        let err = recheck_restart(&state, &ticket).unwrap_err();
        assert!(err.to_string().contains("released"), "{err}");
        assert!(err.to_string().contains("before shutdown"), "{err}");

        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let ticket = begin_restart(&state, &caller("alice")).unwrap();
        handoff(&state, &caller("alice"), "carol").unwrap();
        let err = recheck_restart(&state, &ticket).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("handed off"), "{text}");
        assert!(text.contains("carol"), "{text}");

        let conn = connect(&db_file(&state)).unwrap();
        conn.execute(
            "UPDATE rollout_leases SET expires_at=1 WHERE status='active'",
            [],
        )
        .unwrap();
        let id: i64 = conn
            .query_row(
                "SELECT id FROM rollout_leases WHERE status='active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        let ticket = RestartTicket {
            lease_id: id,
            holder: "carol".into(),
        };
        let err = recheck_restart(&state, &ticket).unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
        let status: String = Connection::open(db_file(&state))
            .unwrap()
            .query_row(
                "SELECT status FROM rollout_leases WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "active",
            "an expired recheck must leave takeover possible"
        );
    }

    #[test]
    fn open_store_reaches_the_current_schema() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.sqlite3");
        Store::open(&db).unwrap();
        let version: i64 = Connection::open(&db)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 19);
    }

    struct MigrationHolder;
    impl Drop for MigrationHolder {
        fn drop(&mut self) {
            MIGRATION_HOLDER.with(|slot| *slot.borrow_mut() = None);
        }
    }
    fn hold_migration(name: &str) -> MigrationHolder {
        MIGRATION_HOLDER.with(|slot| *slot.borrow_mut() = Some(name.to_string()));
        MigrationHolder
    }

    #[test]
    fn gate_log_ingests_only_migration_refusals_and_does_not_replay() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let log = state.join(GATE_LOG);
        let valid = r#"{"kind":"rollout_migration_refused","payload":{"from":11,"to":12,"reason":"no matching backup receipt"},"at":1}"#;
        std::fs::write(
            &log,
            format!(
                "{valid}\n{{\"kind\":\"rollout_release\",\"payload\":{{\"holder\":\"operator:ada\"}},\"at\":1}}\nnot-json\n{{\"kind\":\"rollout_migration_refused\",\"payload\":{{\"from\":\"nope\"}},\"at\":1}}\n"
            ),
        )
        .unwrap();
        let conn = Connection::open(db_file(&state)).unwrap();
        let stats = ingest_gate_log(&state, &conn).unwrap();
        assert_eq!(stats.inserted, 1);
        assert_eq!(stats.skipped, 3);
        assert!(!log.exists());
        let events = events_of(&state);
        let refused: Vec<_> = events
            .iter()
            .filter(|event| event.0 == "rollout_migration_refused")
            .collect();
        assert_eq!(refused.len(), 1, "{events:?}");
        assert!(
            refused[0].1.contains("\"source\":\"gate_log\""),
            "{}",
            refused[0].1
        );
        assert!(events.iter().all(|event| event.0 != "rollout_release"));
        let at: f64 = conn
            .query_row(
                "SELECT at FROM events WHERE kind='rollout_migration_refused'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(at > 1_000_000.0, "line at=1 must not be trusted: {at}");
        let again = ingest_gate_log(&state, &conn).unwrap();
        assert_eq!(again.inserted, 0);

        std::fs::write(&log, format!("{valid}\n{valid}\n")).unwrap();
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER fail_second BEFORE INSERT ON events \
             WHEN (SELECT COUNT(*) FROM events) >= {} \
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            before + 1
        ))
        .unwrap();
        assert!(ingest_gate_log(&state, &conn).is_err());
        assert!(!log.exists());
        let mid: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mid, before + 1);
        let replay = ingest_gate_log(&state, &conn).unwrap();
        assert_eq!(replay.inserted, 0);
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, mid);
    }

    #[test]
    fn backup_refuses_the_live_database_a_symlink_and_an_old_copy() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let db = db_file(&state);
        checkpoint(&db);
        let err = record_backup(&state, &caller("alice"), &db).unwrap_err();
        assert!(err.to_string().contains("live database"), "{err}");
        let link = dir.path().join("link.sqlite3");
        std::os::unix::fs::symlink(&db, &link).unwrap();
        let err = record_backup(&state, &caller("alice"), &link).unwrap_err();
        assert!(err.to_string().contains("live database"), "{err}");
        let dotted = dir.path().join("nested");
        std::fs::create_dir_all(&dotted).unwrap();
        let via_dots = dotted.join("../state/cadence.sqlite3");
        let err = record_backup(&state, &caller("alice"), &via_dots).unwrap_err();
        assert!(err.to_string().contains("live database"), "{err}");
        std::fs::write(sidecar(&db, "-wal"), b"wal-bytes-padding-over-32-bytes!!").unwrap();
        let err = record_backup(&state, &caller("alice"), &sidecar(&db, "-wal")).unwrap_err();
        assert!(err.to_string().contains("live database"), "{err}");
        let old = dir.path().join("old.sqlite3");
        std::fs::copy(&db, &old).unwrap();
        let file = std::fs::File::options().write(true).open(&old).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        drop(file);
        let err = record_backup(&state, &caller("alice"), &old).unwrap_err();
        assert!(
            err.to_string().contains("before this lease was claimed"),
            "{err}"
        );
    }

    #[test]
    fn non_holder_cannot_migrate_even_with_a_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        checkpoint(&db);
        let backup = dir.path().join("backup.sqlite3");
        std::fs::copy(&db, &backup).unwrap();
        record_backup(&state, &caller("alice"), &backup).unwrap();
        checkpoint(&db);
        let before = std::fs::read(&db).unwrap();
        let _intruder = hold_migration("bob");
        let err = authorize_migration(&db).unwrap_err();
        drop(_intruder);
        assert!(err.to_string().contains("backup"), "{err}");
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert!(!sidecar(&db, "-wal").exists());
        let version: i64 = open_peek(&db)
            .unwrap()
            .conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
    }

    #[test]
    fn peek_sees_a_receipt_that_lives_only_in_the_wal() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = downgrade_to_v11(&state);
        checkpoint(&db);
        let main_before = std::fs::read(&db).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0i64)
            .unwrap();
        conn.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
        ensure_lease_tables(&conn).unwrap();
        let now = unix_now();
        conn.execute(
            "INSERT INTO rollout_leases(
                holder, holder_source, host_note, reason, schema_from, schema_to,
                backup_path, backup_sha256, backup_schema, backup_taken_at,
                claimed_at, expires_at, status)
             VALUES('alice','as','','wal',11,12,'/tmp/b','abc',11,?1,?1,?2,'active')",
            params![now, now + 3600.0],
        )
        .unwrap();
        drop(conn);
        assert_eq!(std::fs::read(&db).unwrap(), main_before);
        assert!(wal_nonempty(&db));
        let _holder = hold_migration("alice");
        let permit = authorize_migration(&db).unwrap();
        assert_eq!(permit.crossing.unwrap().reason, "backup_receipt");
        assert_eq!(std::fs::read(&db).unwrap(), main_before);
    }

    #[test]
    fn force_release_records_the_ousted_holder_and_rejects_agent_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let err = release_forced(
            &state,
            &Caller {
                identity: "pane".into(),
                source: "alias",
            },
            "holder died",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("operator"), "{err}");
        Connection::open(db_file(&state))
            .unwrap()
            .execute(
                "INSERT INTO agents(alias, provider, endpoint_kind, role, cwd, sandbox, state, created, updated)
                 VALUES('ada','fake','fake','worker','/','none','stopped',0,0)",
                [],
            )
            .unwrap();
        let err = reject_registered_alias(&state, &caller("ada")).unwrap_err();
        assert!(err.to_string().contains("registered agent alias"), "{err}");
        let err = release(&state, &caller("operator:ada")).unwrap_err();
        assert!(err.to_string().contains("only the holder"), "{err}");
        assert!(
            !err.to_string().contains("release --force"),
            "a live lease must not advertise force-release: {err}"
        );
        let released =
            release_as_plain_operator(&state, "operator:ada", "holder died", Some("alice"))
                .unwrap();
        assert_eq!(released["holder"], "alice");
        assert_eq!(released["forced"], true);
        let events = events_of(&state);
        let release = events
            .iter()
            .find(|event| event.0 == "rollout_release" && event.1.contains("ousted_holder"))
            .map(|event| &event.1)
            .unwrap();
        assert!(release.contains("alice"), "{release}");
        assert!(release.contains("holder died"), "{release}");
        assert!(parse_ttl("8d").unwrap_err().to_string().contains("7 days"));
        assert!(parse_ttl("7d").is_ok());
    }

    #[test]
    fn backup_refuses_a_hard_link_and_a_copy_that_lacks_this_lease_row() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = db_file(&state);
        checkpoint(&db);
        let pre_claim = dir.path().join("pre-claim.sqlite3");
        std::fs::copy(&db, &pre_claim).unwrap();
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        checkpoint(&db);
        let link = dir.path().join("hardlink.sqlite3");
        std::fs::hard_link(&db, &link).unwrap();
        let err = record_backup(&state, &caller("alice"), &link).unwrap_err();
        assert!(err.to_string().contains("hard link or bind mount"), "{err}");
        let wal = sidecar(&db, "-wal");
        std::fs::write(&wal, vec![0u8; 64]).unwrap();
        let wal_link = dir.path().join("wal-hardlink");
        std::fs::hard_link(&wal, &wal_link).unwrap();
        let err = record_backup(&state, &caller("alice"), &wal_link).unwrap_err();
        assert!(err.to_string().contains("hard link or bind mount"), "{err}");
        let _ = std::fs::remove_file(&wal);
        let _ = std::fs::remove_file(sidecar(&db, "-shm"));

        let file = std::fs::File::options()
            .write(true)
            .open(&pre_claim)
            .unwrap();
        file.set_modified(SystemTime::now()).unwrap();
        drop(file);
        let err = record_backup(&state, &caller("alice"), &pre_claim).unwrap_err();
        assert!(
            err.to_string().contains("does not contain this lease"),
            "{err}"
        );

        let stripped = dir.path().join("stripped.sqlite3");
        std::fs::copy(&db, &stripped).unwrap();
        {
            let conn = Connection::open(&stripped).unwrap();
            conn.execute("DELETE FROM rollout_leases", []).unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
        }
        let err = record_backup(&state, &caller("alice"), &stripped).unwrap_err();
        assert!(
            err.to_string().contains("does not contain this lease"),
            "{err}"
        );

        let foreign = dir.path().join("foreign.sqlite3");
        let conn = Connection::open(&foreign).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version(version INTEGER NOT NULL);
             INSERT INTO schema_version(version) VALUES(12);",
        )
        .unwrap();
        drop(conn);
        let err = record_backup(&state, &caller("alice"), &foreign).unwrap_err();
        assert!(
            err.to_string().contains("does not contain this lease"),
            "{err}"
        );
    }

    #[test]
    fn refused_start_does_not_rewrite_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let db = db_file(&state);
        {
            let conn = connect_ensured(&db).unwrap();
            upsert_daemon_build(&conn, "deadbeef", 1.0).unwrap();
        }
        checkpoint(&db);
        let before = sqlite_family(&db);
        let err = authorize_spawn_for(&state, Ok(caller("operator:intruder"))).unwrap_err();
        assert!(err.to_string().contains("deadbeef"), "{err}");
        assert_eq!(sqlite_family(&db), before);
        let log = std::fs::read_to_string(state.join(GATE_LOG)).unwrap();
        assert!(log.contains("rollout_start_refused"), "{log}");
        assert!(events_of(&state)
            .iter()
            .all(|event| event.0 != "rollout_start_refused"));
        let conn = Connection::open(&db).unwrap();
        let stats = ingest_gate_log(&state, &conn).unwrap();
        assert_eq!(stats.inserted, 1);
        let refused = events_of(&state)
            .into_iter()
            .find(|event| event.0 == "rollout_start_refused")
            .unwrap();
        assert!(
            refused.1.contains("\"source\":\"gate_log\""),
            "{}",
            refused.1
        );
        assert!(refused.1.contains("deadbeef"), "{}", refused.1);
    }

    #[test]
    fn forwarded_rollout_identity_is_visible_to_other_threads() {
        // A local lock, not the process-wide `FORWARDED_AS`: that one
        // can never be reset, so setting it here would make later
        // `cargo test --lib rollout` cases order-dependent.
        let slot = std::sync::OnceLock::new();
        remember_forwarded(&slot, Some("operator:thread-proof".into()));
        let seen =
            std::thread::scope(|scope| scope.spawn(|| read_forwarded(&slot)).join().unwrap());
        assert_eq!(seen.as_deref(), Some("operator:thread-proof"));
    }

    #[test]
    fn live_force_release_must_name_the_holder_and_the_hint_is_only_for_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let err = release_forced(&state, &caller("operator:ada"), "oust", None).unwrap_err();
        assert!(err.to_string().contains("--holder"), "{err}");
        assert!(status_at(&state)["held"].as_bool().unwrap(), "{state:?}");
        let err = release_forced(&state, &caller("operator:ada"), "oust", Some("bob")).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        assert_eq!(status_at(&state)["holder"], "alice");
        let err = release(&state, &caller("bob")).unwrap_err();
        assert!(
            !err.to_string().contains("release --force"),
            "healthy lease: {err}"
        );
        let err = begin_restart(&state, &caller("bob")).unwrap_err();
        assert!(
            !err.to_string().contains("release --force"),
            "healthy lease restart: {err}"
        );

        let expired = dir.path().join("expired");
        std::fs::create_dir_all(&expired).unwrap();
        Store::open(&db_file(&expired)).unwrap();
        claim_as(&expired, "alice", 1_000.0, Duration::from_secs(10), false).unwrap();
        let err = release(&expired, &caller("alice")).unwrap_err();
        assert!(err.to_string().contains("--takeover"), "{err}");
        assert!(err.to_string().contains("release --force"), "{err}");
        let err = begin_restart(&expired, &caller("bob")).unwrap_err();
        assert!(err.to_string().contains("release --force"), "{err}");
        let released =
            release_as_plain_operator(&expired, "operator:ada", "expired holder", None).unwrap();
        assert_eq!(released["forced"], true);
        assert_eq!(released["holder"], "alice");
    }

    #[test]
    fn enforce_running_build_fails_closed_when_the_record_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.sqlite3");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE daemon_build(id INTEGER PRIMARY KEY);")
            .unwrap();
        let err = enforce_running_build(&conn).unwrap_err();
        assert!(
            err.to_string()
                .contains("could not read the recorded build"),
            "{err}"
        );
    }

    fn sqlite_family(db: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        ["", "-wal", "-shm"]
            .into_iter()
            .map(|suffix| {
                let path = if suffix.is_empty() {
                    db.to_path_buf()
                } else {
                    sidecar(db, suffix)
                };
                (suffix.to_string(), std::fs::read(path).ok())
            })
            .collect()
    }

    fn events_of(state: &Path) -> Vec<(String, String)> {
        let conn = Connection::open(db_file(state)).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT kind, payload FROM events WHERE alias='daemon' AND kind LIKE 'rollout_%'",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn status_at(state: &Path) -> Value {
        super::status(state).unwrap()
    }

    /// Success-path force-release from a process `operator_proof` accepts:
    /// reparented to init, `CADENCE_ALIAS` removed, stdio off any pane pty.
    fn release_as_plain_operator(
        state: &Path,
        identity: &str,
        reason: &str,
        holder: Option<&str>,
    ) -> Result<Value> {
        run_operator_probe(state, identity, reason, holder, true)
    }

    fn run_operator_probe(
        state: &Path,
        identity: &str,
        reason: &str,
        holder: Option<&str>,
        reparent: bool,
    ) -> Result<Value> {
        if let Some(home) = std::env::var_os("HOME") {
            let live = PathBuf::from(home).join(".local/state/cadence");
            assert!(
                !state.starts_with(&live),
                "refusing to probe the live state dir {}",
                live.display()
            );
        }
        let scratch = tempfile::tempdir().unwrap();
        let req = scratch.path().join("req.json");
        let pid_path = scratch.path().join("pid");
        let out_path = scratch.path().join("out.json");
        let err_path = scratch.path().join("err");
        std::fs::write(
            &req,
            serde_json::json!({
                "state": state,
                "identity": identity,
                "reason": reason,
                "holder": holder,
            })
            .to_string(),
        )
        .unwrap();
        let exe = std::env::current_exe().unwrap();
        let mut cmd = if reparent {
            let mut cmd = std::process::Command::new("setsid");
            cmd.arg("-f")
                .arg("env")
                .arg("-u")
                .arg("CADENCE_ALIAS")
                .arg("-u")
                .arg("CADENCE_STATE_DIR")
                .arg(&exe);
            cmd
        } else {
            let mut cmd = std::process::Command::new("env");
            cmd.arg("-u")
                .arg("CADENCE_ALIAS")
                .arg("-u")
                .arg("CADENCE_STATE_DIR")
                .arg(&exe);
            cmd
        };
        let err_file = std::fs::File::create(&err_path).unwrap();
        cmd.args([
            "--ignored",
            "rollout_operator_shell_probe",
            "--test-threads",
            "1",
        ])
        .env("CADENCE_ROLLOUT_OPERATOR_PROBE", &req)
        .env("CADENCE_ROLLOUT_OPERATOR_PROBE_PID", &pid_path)
        .env("CADENCE_ROLLOUT_OPERATOR_PROBE_OUT", &out_path)
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_STATE_DIR")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(err_file));
        if reparent {
            let status = cmd.status().unwrap();
            assert!(status.success(), "setsid exited {status}");
            wait_for_operator_probe(&pid_path, &out_path, &err_path)
        } else {
            let status = cmd.status().unwrap();
            let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
            assert!(status.success(), "probe harness failed: {stderr}");
            let text = std::fs::read_to_string(&out_path)
                .unwrap_or_else(|error| panic!("probe wrote no result ({error}): {stderr}"));
            decode_operator_probe(&text)
        }
    }

    fn wait_for_operator_probe(pid_path: &Path, out_path: &Path, err_path: &Path) -> Result<Value> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut pid = None;
        loop {
            if pid.is_none() {
                pid = std::fs::read_to_string(pid_path)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok());
            }
            if out_path.exists() {
                let text = std::fs::read_to_string(out_path).unwrap();
                reap_operator_probe(pid);
                return decode_operator_probe(&text);
            }
            if std::time::Instant::now() > deadline {
                reap_operator_probe(pid);
                let stderr = std::fs::read_to_string(err_path).unwrap_or_default();
                panic!("operator probe timed out: {stderr}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn reap_operator_probe(pid: Option<u32>) {
        let Some(pid) = pid else {
            return;
        };
        if pid <= 1 || pid == std::process::id() {
            return;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && unsafe { libc::kill(pid as i32, 0) } == 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }

    fn decode_operator_probe(text: &str) -> Result<Value> {
        let payload: Value = serde_json::from_str(text).unwrap();
        if payload["ok"].as_bool() == Some(true) {
            Ok(payload["value"].clone())
        } else {
            Err(Error::rejected(
                payload["error"]
                    .as_str()
                    .unwrap_or("operator probe failed")
                    .to_string(),
            ))
        }
    }

    fn register_pane(state: &Path, alias: &str, pid: u32) {
        Connection::open(db_file(state))
            .unwrap()
            .execute(
                "INSERT INTO agents(alias, provider, endpoint_kind, role, cwd, sandbox, \
                 state, created, updated, pid, generation) \
                 VALUES(?1,'cursor','pty','worker','/tmp','none','idle',0,0,?2,'g1')",
                rusqlite::params![alias, i64::from(pid)],
            )
            .unwrap();
    }

    fn write_enrolled_root(state: &Path, pid: u32) {
        let starttime = proc_starttime(pid).unwrap();
        let doc = serde_json::json!({
            "format": "cadence-slots",
            "version": 2,
            "enrollments": [{
                "root": {"pid": pid, "starttime": starttime, "uid": unsafe { libc::getuid() }}
            }]
        });
        std::fs::write(state.join("slots.json"), doc.to_string()).unwrap();
    }

    /// Re-exec target for [`run_operator_probe`]. Ignored in a normal
    /// run; the probe sets `CADENCE_ROLLOUT_OPERATOR_PROBE` and passes
    /// `--ignored`.
    #[test]
    #[ignore = "re-exec helper for force-release operator proof"]
    fn rollout_operator_shell_probe() {
        let Ok(req_path) = std::env::var("CADENCE_ROLLOUT_OPERATOR_PROBE") else {
            return;
        };
        let pid_path = std::env::var("CADENCE_ROLLOUT_OPERATOR_PROBE_PID").unwrap();
        let out_path = std::env::var("CADENCE_ROLLOUT_OPERATOR_PROBE_OUT").unwrap();
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();
        let req: Value = serde_json::from_str(&std::fs::read_to_string(req_path).unwrap()).unwrap();
        let state = PathBuf::from(req["state"].as_str().unwrap());
        let released = release_forced(
            &state,
            &caller(req["identity"].as_str().unwrap()),
            req["reason"].as_str().unwrap(),
            req["holder"].as_str(),
        );
        let payload = match released {
            Ok(value) => serde_json::json!({"ok": true, "value": value}),
            Err(error) => serde_json::json!({"ok": false, "error": error.to_string()}),
        };
        // The reparented parent polls for `out.json` and reads it the
        // moment it exists; `fs::write` creates the file before it
        // writes, so publish by rename — the parent sees all or nothing
        // (CAD-421).
        let partial = format!("{out_path}.partial");
        std::fs::write(&partial, payload.to_string()).unwrap();
        std::fs::rename(&partial, out_path).unwrap();
    }

    #[test]
    fn daemon_pid_for_proof_is_zero_when_stopped_and_the_lock_holder_when_running() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        std::fs::create_dir_all(state).unwrap();
        assert_eq!(daemon_pid_for_proof(state).unwrap(), 0);
        let chain = crate::adapter::pty::caller_chain(std::process::id()).unwrap();
        assert!(
            !chain.contains(&0),
            "sentinel 0 is on the ancestry: {chain:?}"
        );
        let path = state.join("cadence.lock");
        std::fs::write(&path, "").unwrap();
        // Released with `flock`, not by close: a sibling test's fork
        // shares the descriptor until its child execs, and a closed
        // holder would still read as the daemon (CAD-389).
        let held = crate::worktree::TestFileLock::acquire(&path);
        assert_eq!(daemon_pid_for_proof(state).unwrap(), std::process::id());
        held.release();
        assert_eq!(daemon_pid_for_proof(state).unwrap(), 0);
    }

    #[test]
    fn flock_holder_rereads_proc_locks_that_skipped_a_live_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cadence.lock");
        std::fs::write(&path, "").unwrap();
        let held = crate::worktree::TestFileLock::acquire(&path);
        // First read stands in for a page boundary that lost the line.
        let mut reads = 0;
        let holder = flock_holder_from(&path, || {
            reads += 1;
            if reads == 1 {
                Ok(String::new())
            } else {
                std::fs::read_to_string("/proc/locks")
            }
        })
        .unwrap();
        assert_eq!(holder, Some(std::process::id()));
        // A real read under lock churn can skip the line too.
        assert!(reads >= 2, "{reads}");

        let mut reads = 0;
        let err = flock_holder_from(&path, || {
            reads += 1;
            Ok(String::new())
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("is locked but its holder is not in /proc/locks"),
            "{err}"
        );
        assert_eq!(reads, PROC_LOCKS_READS);
        held.release();
    }

    #[test]
    fn force_release_refuses_a_caller_whose_ancestry_includes_a_pane() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        register_pane(&state, "pane-a", std::process::id());
        let err = run_operator_probe(&state, "operator:ada", "from a pane", Some("alice"), false)
            .unwrap_err();
        assert!(
            err.to_string().contains("on its ancestry") && err.to_string().contains("pane-a"),
            "{err}"
        );
        assert!(status_at(&state)["held"].as_bool().unwrap(), "{err}");
    }

    /// CAD-385: the operator-proof pane deny list checks each row's
    /// recorded start time. A row whose pid now names a different
    /// process drops out (as if unregistered); a row with no recorded
    /// start stays (fail closed) — and on a store older than v14, which
    /// has no `pid_start` column, every row is such a row.
    #[test]
    fn registered_panes_drop_a_reused_pid_and_keep_a_row_without_start() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        let me = std::process::id();
        let parent = std::os::unix::process::parent_id();
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let conn = Connection::open(db_file(&state)).unwrap();
        for (alias, pid, start) in [
            ("live", me, proc_starttime(me).map(|t| t as i64)),
            (
                "reused",
                parent,
                proc_starttime(parent).map(|t| t as i64 - 1),
            ),
            ("legacy", other.id(), None),
        ] {
            conn.execute(
                "INSERT INTO agents(alias, provider, endpoint_kind, role, cwd, sandbox, \
                 state, created, updated, pid, pid_start, generation) \
                 VALUES(?1,'cursor','pty','worker','/tmp','none','idle',0,0,?2,?3,'g1')",
                rusqlite::params![alias, i64::from(pid), start],
            )
            .unwrap();
        }
        assert_eq!(
            registered_panes(&state).unwrap(),
            HashMap::from([(me, "live".to_string()), (other.id(), "legacy".to_string())])
        );
        conn.execute_batch("ALTER TABLE agents DROP COLUMN pid_start")
            .unwrap();
        drop(conn);
        assert_eq!(registered_panes(&state).unwrap().len(), 3);
        let _ = other.kill();
        let _ = other.wait();
    }

    #[test]
    fn force_release_refuses_an_enrolled_root_on_the_ancestry() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        write_enrolled_root(&state, std::process::id());
        let err = run_operator_probe(
            &state,
            "operator:ada",
            "from an endpoint",
            Some("alice"),
            false,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("enrolled managed endpoint"),
            "{err}"
        );
        assert!(status_at(&state)["held"].as_bool().unwrap(), "{err}");
    }

    #[test]
    fn force_release_accepts_a_plain_operator_when_the_daemon_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        assert_eq!(daemon_pid_for_proof(&state).unwrap(), 0);
        let released =
            release_as_plain_operator(&state, "operator:ada", "daemon stopped", Some("alice"))
                .unwrap();
        assert_eq!(released["forced"], true);
        assert_eq!(released["holder"], "alice");
        assert!(!status_at(&state)["held"].as_bool().unwrap());
    }

    #[test]
    fn force_release_accepts_a_plain_operator_when_the_daemon_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(
            &state,
            "alice",
            unix_now(),
            Duration::from_secs(3600),
            false,
        )
        .unwrap();
        let path = state.join("cadence.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        use std::os::unix::io::AsRawFd;
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert_eq!(daemon_pid_for_proof(&state).unwrap(), std::process::id());
        let released =
            release_as_plain_operator(&state, "operator:ada", "daemon running", Some("alice"))
                .unwrap();
        drop(file);
        assert_eq!(released["forced"], true);
        assert_eq!(released["holder"], "alice");
        assert!(!status_at(&state)["held"].as_bool().unwrap());
    }
}
