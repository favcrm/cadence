//! Rollout migration: see the parent module.

use super::*;

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

pub(super) fn migration_refusal(version: i64, lease_table: bool) -> String {
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
            revoked_by TEXT);
         -- CAD-1024: the staging-delegation allowlist and its grants.
         -- `staging_instances` marks a state dir as staging (operator only;
         -- `staging_register` writes it after the production allowlist).
         -- `staging_grants` is the grant store — it admits nothing until
         -- PR-3 adds the `delegate:` caller shape and `require_delegate_grant`.
         CREATE TABLE IF NOT EXISTS staging_instances(
            id INTEGER PRIMARY KEY CHECK (id = 1),
            state_dir TEXT NOT NULL,
            board_port INTEGER NOT NULL,
            registered_by TEXT NOT NULL,
            registered_at REAL NOT NULL);
         CREATE TABLE IF NOT EXISTS staging_grants(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            alias TEXT NOT NULL,
            ops TEXT NOT NULL,
            state_dir TEXT NOT NULL,
            granted_by TEXT NOT NULL,
            granted_at REAL NOT NULL,
            expires_at REAL NOT NULL,
            revoked_at REAL,
            revoked_by TEXT,
            last_used_at REAL);",
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

/// A dev store takes no part in a rollout: it is disposable, so a
/// rebuilt binary starts it, and migrates it, without the lease
/// (CAD-310). CAD-1187: the mode is the store's — it must hold the dev
/// marker `cadence dev up` writes into it and sit under the sandbox
/// base (`sandbox::dev_owner`). The caller's `CADENCE_PROFILE` is not
/// consulted, so a hand-set profile on any other store unlocks nothing;
/// production's rules are unchanged.
pub fn sandbox_exempt(state_dir: &Path) -> bool {
    matches!(crate::sandbox::dev_owner(state_dir), Ok(Some(_)))
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
    // CAD-1024: a `delegate:<alias>` start needs a live grant for
    // `daemon_start` on this dir; an `operator:`/`alias` caller is unchanged.
    if caller.source == "delegate" {
        require_delegate_grant(state_dir, &caller, "daemon_start")?;
    }
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
