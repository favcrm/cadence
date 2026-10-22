//! Rollout gate: see the parent module.

use super::*;

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
pub(super) fn gate_line(line: &str) -> Option<(&'static str, Value)> {
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

pub(super) fn nonempty(text: &str) -> Option<&str> {
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

pub(super) fn claim_in(conn: &Connection, req: &ClaimRequest<'_>) -> Result<TxResult<Value>> {
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

pub(super) fn insert_lease(
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

pub(super) fn require_holder(
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
pub(super) fn holder_block(conn: &Connection, caller: &Caller, now: f64) -> Result<Option<String>> {
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
pub(super) fn with_expired_hint(lease: &Lease, now: f64, body: String) -> String {
    if lease.expires_at <= now {
        format!("{body} {FORCE_RELEASE_HINT}")
    } else {
        body
    }
}

pub(super) fn qualifying_receipt(path: &Path, version: i64, holder: Option<&str>) -> Result<bool> {
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

pub(super) fn held_status(lease: &Lease, now: f64) -> Value {
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

pub(super) fn backup_json(lease: &Lease) -> Value {
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

pub(super) fn target_of(lease: &Lease) -> String {
    lease.target_commit.clone().unwrap_or_else(|| "none".into())
}

pub(super) fn fmt_epoch(epoch: f64) -> String {
    format!("{epoch:.3}")
}

pub(super) fn validate_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() || reason.len() > 500 || reason.chars().any(|c| c.is_control()) {
        return Err(Error::rejected(
            "--reason must be 1..=500 characters with no control characters",
        ));
    }
    Ok(())
}

pub(super) fn validate_target(target: &str) -> Result<()> {
    let ok = (7..=64).contains(&target.len()) && target.chars().all(|c| c.is_ascii_hexdigit());
    if !ok {
        return Err(Error::rejected("--target must be 7..=64 hex characters"));
    }
    Ok(())
}

pub(super) fn host_note() -> String {
    let host =
        std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_else(|_| "unknown".into());
    format!("host={} pid={}", host.trim(), std::process::id())
}

pub(super) fn canonical_backup(state_dir: &Path, backup: &Path) -> Result<PathBuf> {
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

pub(super) fn same_file(a: &Path, b: &Path) -> bool {
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
pub(super) fn backup_contains_lease(backup: &Path, lease: &Lease) -> Result<bool> {
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

pub(super) fn file_mtime(path: &Path) -> Result<f64> {
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

pub(super) fn inspect_backup(path: &Path) -> Result<(String, i64)> {
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

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// CAD-1011: rollout sibling-writers must not mutate a durable-sealed
/// `cadence.sqlite3`. Before opening for write, run the same read-only
/// closure-latch preflight the `Store` open path runs — a latched
/// (sealed or open-latch) file refuses; a latch-absent legacy db may
/// proceed. This keeps a rollout `connect`/`immediate`/`connect_ensured`
/// from silently writing a protected store through an unguarded
/// `Connection`.
pub(super) fn connect(path: &Path) -> Result<Connection> {
    crate::store::preflight_writer_guard(path)?;
    let conn = Connection::open(path)?;
    conn.busy_timeout(store::BUSY_TIMEOUT)?;
    Ok(conn)
}

pub(super) fn connect_ensured(path: &Path) -> Result<Connection> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = connect(path)?;
    immediate(&conn, ensure_lease_tables)?;
    Ok(conn)
}

pub(super) fn immediate<T>(
    conn: &Connection,
    body: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    // SQLite serializes the held latch check with every other connection's
    // close. RAII also rolls back callback errors and unwinds before reuse.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    crate::store::require_legacy_writer_tx(&tx)?;
    let value = body(&tx)?;
    tx.commit()?;
    Ok(value)
}

pub(super) fn active_lease(conn: &Connection) -> Result<Option<Lease>> {
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

pub(super) fn lease_by_id(conn: &Connection, id: i64) -> Result<Option<Lease>> {
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

pub(super) fn row_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
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

pub(super) fn mark_ended(
    conn: &Connection,
    id: i64,
    status: &str,
    reason: &str,
    at: f64,
) -> Result<()> {
    conn.execute(
        "UPDATE rollout_leases SET status=?1, end_reason=?2, ended_at=?3 WHERE id=?4",
        params![status, reason, at, id],
    )?;
    Ok(())
}

pub(super) fn read_build(conn: &Connection) -> Result<Option<String>> {
    if !table_exists(conn, "daemon_build")? {
        return Ok(None);
    }
    conn.query_row("SELECT commit_sha FROM daemon_build WHERE id=1", [], |r| {
        r.get(0)
    })
    .optional()
    .map_err(Into::into)
}

pub(super) fn read_user_schema(conn: &Connection) -> Result<Option<i64>> {
    if !table_exists(conn, "schema_version")? {
        return Ok(None);
    }
    let version: i64 = conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
    Ok(Some(version))
}

pub(super) fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

pub(super) fn insert_event(conn: &Connection, kind: &str, payload: Value, at: f64) -> Result<()> {
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

pub(super) fn column_names(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(super) enum Peek {
    Missing,
    Fresh,
    Version(i64),
}

pub(super) struct PeekConn {
    pub(super) _tmp: Option<tempfile::TempDir>,
    pub(super) conn: Connection,
}

pub(super) fn peek_schema(path: &Path) -> Result<Peek> {
    if !path.exists() {
        return Ok(Peek::Missing);
    }
    match peek_user_schema(path)? {
        None | Some(0) => Ok(Peek::Fresh),
        Some(version) => Ok(Peek::Version(version)),
    }
}

pub(super) fn peek_user_schema(path: &Path) -> Result<Option<i64>> {
    let peek = open_peek(path)?;
    if !table_exists(&peek.conn, "schema_version")? {
        return Ok(None);
    }
    let version: i64 = peek
        .conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
    Ok(Some(version))
}

pub(super) fn lease_table_present(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let peek = open_peek(path)?;
    table_exists(&peek.conn, "rollout_leases")
}

/// Read the database without writing it. A non-empty `-wal` is copied
/// with its sidecars so a lease that exists only in the WAL is visible;
/// `immutable=1` would ignore that WAL.
pub(super) fn open_peek(path: &Path) -> Result<PeekConn> {
    if wal_nonempty(path) {
        return open_peek_copy(path);
    }
    match open_immutable(path) {
        Ok(conn) => Ok(PeekConn { _tmp: None, conn }),
        Err(_) => open_peek_copy(path),
    }
}

pub(super) fn wal_nonempty(path: &Path) -> bool {
    std::fs::metadata(sidecar(path, "-wal"))
        .map(|meta| meta.len() > 32)
        .unwrap_or(false)
}

pub(super) fn open_peek_copy(path: &Path) -> Result<PeekConn> {
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

pub(super) fn open_immutable(path: &Path) -> Result<Connection> {
    let uri = format!("file:{}?mode=ro&immutable=1", encode_uri_path(path));
    Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(Into::into)
}

pub(super) fn encode_uri_path(path: &Path) -> String {
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

pub(super) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

pub(super) fn append_gate_log(db: &Path, kind: &str, payload: Value) -> Result<()> {
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
