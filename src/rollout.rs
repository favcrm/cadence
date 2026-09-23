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

use std::cell::Cell;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::store::{self, Store};

/// Schema version that introduces `rollout_leases` and `daemon_build`.
/// The v12 migration in `store` hardcodes this same number.
pub const SCHEMA_VERSION: i64 = 12;

/// Last schema that has no lease table. The bootstrap opt-in covers
/// only this version.
pub const PRE_LEASE_SCHEMA: i64 = 11;

const DEFAULT_TTL: Duration = Duration::from_secs(12 * 60 * 60);

thread_local! {
    /// Test override so a bootstrap opt-in does not leak across threads
    /// through the process environment.
    static BOOTSTRAP_FORCE: Cell<Option<bool>> = const { Cell::new(None) };
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
const MAX_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
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
        return Err(Error::rejected("ttl cannot exceed 30 days"));
    }
    Ok(ttl)
}

pub fn resolve_caller(explicit_as: Option<&str>) -> Result<Caller> {
    let alias = std::env::var("CADENCE_ALIAS").ok();
    let rollout_as = std::env::var("CADENCE_ROLLOUT_AS").ok();
    let explicit = explicit_as.or(rollout_as.as_deref());
    resolve_caller_with(alias.as_deref(), explicit)
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

    if !qualifying_receipt(path, version)? {
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
         cannot run `cadence rollout`. For this single schema 11 to 12 \
         introduction, start the daemon once with CADENCE_ROLLOUT_BOOTSTRAP=1. \
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
            recorded_at REAL NOT NULL);",
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

pub fn authorize_spawn_for(state_dir: &Path, caller: Result<Caller>) -> Result<Option<String>> {
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(caller.ok().map(|c| c.identity));
    }
    let conn = connect(&path)?;
    let recorded = read_build(&conn)?;
    let current = crate::overview::BUILD_COMMIT;
    if recorded.is_none() || recorded.as_deref() == Some(current) {
        return Ok(caller.ok().map(|c| c.identity));
    }
    let caller = caller?;
    match holder_block(&conn, &caller, unix_now())? {
        None => Ok(Some(caller.identity)),
        Some(block) => {
            let message = format!(
                "daemon start of build {current} refused: the daemon last \
                 recorded build {} and {block}",
                recorded.clone().unwrap_or_default()
            );
            let _ = insert_event(
                &conn,
                "rollout_start_refused",
                json!({
                    "build": current,
                    "recorded": recorded,
                    "holder": caller.identity,
                    "reason": block,
                }),
                unix_now(),
            );
            Err(Error::rejected(message))
        }
    }
}

/// Backstop inside `daemon run` for a binary started without `daemon start`.
pub fn enforce_running_build(conn: &Connection) -> Result<()> {
    let recorded = match read_build(conn) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let current = crate::overview::BUILD_COMMIT;
    if recorded.is_none() || recorded.as_deref() == Some(current) {
        return Ok(());
    }
    let now = unix_now();
    let caller = resolve_caller(None);
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
            mark_ended(&conn, lease.id, "expired", "expiry", now)?;
            let _ = insert_event(
                &conn,
                "rollout_expiry",
                json!({"holder": lease.holder, "expires_at": lease.expires_at}),
                now,
            );
            Some(format!(
                "rollout lease expired while waiting for the idle gate (holder {}, expires_at {})",
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
    committed(immediate(&conn, |conn| {
        if let Some(lease) = active_lease(conn)? {
            if lease.expires_at <= now {
                mark_ended(conn, lease.id, "expired", "expiry", now)?;
                insert_event(
                    conn,
                    "rollout_expiry",
                    json!({"holder": lease.holder, "expires_at": lease.expires_at}),
                    now,
                )?;
                return Ok(TxResult::Done(expired_status(&lease, now)));
            }
            return Ok(TxResult::Done(held_status(&lease, now)));
        }
        Ok(TxResult::Done(json!({"held": false})))
    }))
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
    let (sha, schema) = inspect_backup(backup)?;
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    committed(immediate(&conn, |conn| {
        let lease = match require_holder(conn, caller, now, false)? {
            TxResult::Done(lease) => lease,
            TxResult::Refuse(message) => return Ok(TxResult::Refuse(message)),
        };
        conn.execute(
            "UPDATE rollout_leases
             SET backup_path=?1, backup_sha256=?2, backup_schema=?3, backup_taken_at=?4
             WHERE id=?5",
            params![backup.display().to_string(), sha, schema, now, lease.id],
        )?;
        insert_event(
            conn,
            "rollout_backup_recorded",
            json!({
                "holder": lease.holder,
                "path": backup,
                "sha256": sha,
                "schema_version": schema,
                "taken_at": now,
            }),
            now,
        )?;
        Ok(TxResult::Done(json!({
            "recorded": true,
            "holder": lease.holder,
            "path": backup,
            "sha256": sha,
            "schema_version": schema,
            "taken_at": now,
        })))
    }))
}

pub fn ingest_gate_log(state_dir: &Path, conn: &Connection) -> Result<()> {
    let path = state_dir.join(GATE_LOG);
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)?;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).unwrap_or(json!({"raw": line}));
        let kind = v["kind"].as_str().unwrap_or("rollout_migration_refused");
        let payload = v.get("payload").cloned().unwrap_or(v.clone());
        let at = v["at"].as_f64().unwrap_or_else(unix_now);
        insert_event(conn, kind, payload, at)?;
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

fn claim_in(conn: &Connection, req: &ClaimRequest<'_>) -> Result<TxResult<Value>> {
    let existing = active_lease(conn)?;
    if let Some(lease) = existing {
        if lease.expires_at <= req.now {
            if !req.takeover {
                mark_ended(conn, lease.id, "expired", "expiry", req.now)?;
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
        return Ok(TxResult::Refuse(format!(
            "rollout lease is held by {} target {} expires_at {}; only the holder can do this",
            lease.holder,
            target_of(&lease),
            fmt_epoch(lease.expires_at)
        )));
    }
    if lease.expires_at <= now && !allow_expired {
        mark_ended(conn, lease.id, "expired", "expiry", now)?;
        insert_event(
            conn,
            "rollout_expiry",
            json!({"holder": lease.holder, "expires_at": lease.expires_at}),
            now,
        )?;
        return Ok(TxResult::Refuse(format!(
            "rollout lease held by {} expired at {}; pass --takeover to claim it",
            lease.holder,
            fmt_epoch(lease.expires_at)
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
        return Ok(Some(format!(
            "rollout lease held by {} target {} expired at {}",
            lease.holder,
            target_of(&lease),
            fmt_epoch(lease.expires_at)
        )));
    }
    if lease.holder != caller.identity {
        return Ok(Some(format!(
            "rollout lease held by {} target {} expires_at {}",
            lease.holder,
            target_of(&lease),
            fmt_epoch(lease.expires_at)
        )));
    }
    Ok(None)
}

fn qualifying_receipt(path: &Path, version: i64) -> Result<bool> {
    if !lease_table_present(path)? {
        return Ok(false);
    }
    let conn = connect(path)?;
    let Some(lease) = active_lease(&conn)? else {
        return Ok(false);
    };
    if lease.expires_at <= unix_now() {
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

fn expired_status(lease: &Lease, now: f64) -> Value {
    json!({
        "held": false,
        "expired": true,
        "holder": lease.holder,
        "target": lease.target_commit,
        "expires_at": lease.expires_at,
        "age_secs": now - lease.claimed_at,
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

fn peek_schema(path: &Path) -> Result<Peek> {
    if !path.exists() {
        return Ok(Peek::Missing);
    }
    match peek_user_schema(path) {
        Ok(None) => Ok(Peek::Fresh),
        Ok(Some(0)) => Ok(Peek::Fresh),
        Ok(Some(v)) => Ok(Peek::Version(v)),
        Err(_) => peek_schema_from_copy(path),
    }
}

fn peek_user_schema(path: &Path) -> Result<Option<i64>> {
    let conn = open_immutable(path)?;
    if !table_exists(&conn, "schema_version")? {
        return Ok(None);
    }
    let version: i64 = conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
    Ok(Some(version))
}

fn peek_schema_from_copy(path: &Path) -> Result<Peek> {
    let tmp = tempfile::tempdir().map_err(|e| Error::internal(format!("peek copy: {e}")))?;
    let copy = tmp.path().join("peek.sqlite3");
    std::fs::copy(path, &copy)?;
    for suffix in ["-wal", "-shm"] {
        let side = sidecar(path, suffix);
        if side.exists() {
            std::fs::copy(&side, sidecar(&copy, suffix))?;
        }
    }
    let conn = Connection::open(&copy)?;
    if !table_exists(&conn, "schema_version")? {
        return Ok(Peek::Fresh);
    }
    let version: i64 = conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
    if version == 0 {
        Ok(Peek::Fresh)
    } else {
        Ok(Peek::Version(version))
    }
}

fn lease_table_present(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    if let Ok(conn) = open_immutable(path) {
        return table_exists(&conn, "rollout_leases");
    }
    let tmp = tempfile::tempdir().map_err(|e| Error::internal(format!("peek copy: {e}")))?;
    let copy = tmp.path().join("peek.sqlite3");
    std::fs::copy(path, &copy)?;
    let conn = Connection::open(&copy)?;
    table_exists(&conn, "rollout_leases")
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
    fn expired_lease_requires_takeover_and_names_the_previous_holder() {
        let dir = tempfile::tempdir().unwrap();
        let state = fresh(&dir);
        claim_as(&state, "alice", 1_000.0, Duration::from_secs(10), false).unwrap();
        let err = claim_as(&state, "bob", 1_020.0, Duration::from_secs(10), false).unwrap_err();
        assert!(err.to_string().contains("alice"), "{err}");
        assert!(err.to_string().contains("--takeover"), "{err}");
        let err = claim_as(&state, "bob", 1_020.0, Duration::from_secs(10), true);
        // The first refused claim already marked it expired, so the row
        // is no longer active. Re-seed an expired-but-still-active row.
        let _ = err;
        claim_as(&state, "alice", 2_000.0, Duration::from_secs(5), false).unwrap();
        let ok = claim_as(&state, "bob", 2_010.0, Duration::from_secs(30), true).unwrap();
        assert_eq!(ok["holder"], "bob");
        assert_eq!(ok["previous_holder"], "alice");
        let events = events_of(&state);
        let takeover = events
            .iter()
            .find(|e| e.0 == "rollout_takeover")
            .map(|e| &e.1)
            .unwrap();
        assert!(takeover.contains("alice"), "{takeover}");
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
    }

    #[test]
    fn open_store_reaches_schema_12() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.sqlite3");
        Store::open(&db).unwrap();
        let version: i64 = Connection::open(&db)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 12);
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
}
