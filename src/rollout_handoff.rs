//! Rollout handoff: see the parent module.

use super::*;

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
