//! Rollout lease: see the parent module.

use super::*;

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
    // CAD-1024: a `delegate:<alias>` holder is admitted only by a live grant
    // for (alias, `rollout_claim`, this dir) — never by operator proof.
    if req.caller.source == "delegate" {
        require_delegate_grant(state_dir, req.caller, "rollout_claim")?;
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
pub(super) fn force_holder_refusal(lease: &Lease, named: Option<&str>, now: f64) -> Option<String> {
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
pub(super) fn preview_force_refusal(
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
