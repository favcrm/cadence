//! Rollout grants: see the parent module.

use super::*;

/// The holder of the active, unexpired rollout lease — when that
/// holder also holds a live operator grant (CAD-384). The daemon's
/// `shutdown` caller rule admits that agent's own pane: the rollout
/// owner restarts from its pane, but only because the operator granted
/// it (`cadence rollout grant`). A lease without a live grant — an
/// agent's handoff, a grant since revoked or expired — admits nobody.
/// Read-only; no lease table (or no database yet) is no holder.
///
/// CAD-1024: a lease held by `delegate:<alias>` admits that delegate's bare
/// `alias` for `daemon stop` when a live `staging_grants` row covers
/// `daemon_stop` on this dir — returned as `alias` so the `Rule::Shutdown`
/// comparison `lease_holder == caller_alias` admits the delegate's own pane.
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
    if let Some(alias) = lease.holder.strip_prefix("delegate:") {
        if staging_grant_covers(&conn, state_dir, alias, "daemon_stop", now)? {
            return Ok(Some(alias.to_string()));
        }
        return Ok(None);
    }
    Ok(live_grant(&conn, &lease.holder, now)?.map(|_| lease.holder))
}

/// A live `staging_grants` row names `(alias, op)` on `state_dir` — the
/// delegate-shutdown leg of [`granted_lease_holder`] and a shared read for
/// the seams PR-3 adds.
pub(super) fn staging_grant_covers(
    conn: &Connection,
    state_dir: &Path,
    alias: &str,
    op: &str,
    now: f64,
) -> Result<bool> {
    if !table_exists(conn, "staging_grants")? {
        return Ok(false);
    }
    let canonical = crate::sandbox::resolved(state_dir)
        .to_string_lossy()
        .into_owned();
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM staging_grants g, json_each(g.ops) o
          WHERE g.alias=?1 AND g.state_dir=?2 AND o.value=?3
            AND g.revoked_at IS NULL AND g.expires_at > ?4)",
        params![alias, canonical, op, now],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// The live grant for `alias` — not revoked, not expired — as
/// `(granted_by, granted_at, expires_at)`.
pub(super) fn live_grant(
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
pub(super) fn live_grants(conn: &Connection, now: f64) -> Result<Vec<Value>> {
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

/// The v1 staging-delegation ops allowlist (CAD-1024). `app_upgrade` is
/// deliberately absent — app-only updates already use the authorized
/// installation API, and `app_workspace_upgrade` is `operator_connection`-
/// gated (no delegate seam exists in v1). Order is fixed for display.
pub const STAGING_OPS: &[&str] = &[
    "rollout_claim",
    "daemon_start",
    "daemon_stop",
    "ui_start",
    "ui_stop",
];

/// Register `state_dir` as a staging instance — the allowlist a grant
/// requires (CAD-1024, contract I2/I5). Operator-only: the caller gate
/// (`operator_connection`) runs in `staging_register`'s RPC handler before
/// this. The production allowlist check happens here so a grant can never be
/// created for a dir that is, or overlaps, the production state/PM paths.
pub fn staging_register(state_dir: &Path, board_port: u16, by: &str) -> Result<Value> {
    refuse_production_staging(state_dir)?;
    if !(3110..=3199).contains(&board_port) {
        return Err(Error::rejected(format!(
            "staging board port {board_port} is not in 3110–3199"
        )));
    }
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    let canonical = canonical_state_dir(state_dir)?;
    let canonical_str = canonical.to_string_lossy().into_owned();
    committed(immediate(&conn, |conn| {
        conn.execute(
            "INSERT INTO staging_instances(id,state_dir,board_port,registered_by,registered_at)
             VALUES(1,?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET state_dir=excluded.state_dir,
                board_port=excluded.board_port, registered_by=excluded.registered_by,
                registered_at=excluded.registered_at",
            params![canonical_str, i64::from(board_port), by, now],
        )?;
        insert_event(
            conn,
            "staging_register",
            json!({"state_dir": canonical_str, "board_port": board_port, "by": by}),
            now,
        )?;
        Ok(TxResult::Done(json!({
            "registered": true, "state_dir": canonical_str, "board_port": board_port,
        })))
    }))
}

/// The production paths a staging registration — and so any grant — must
/// never touch (CAD-1024, contract I5). Canonicalized, so a symlinked or
/// `..`-spelled staging dir cannot alias a live one. `~/pm` is the
/// production PM dir; `client::default_state_dir()`/`home::local_state_dir()`
/// are the two spellings of the production state dir.
pub(super) fn refuse_production_staging(state_dir: &Path) -> Result<()> {
    let ours = crate::sandbox::resolved(state_dir);
    let mut forbidden: Vec<PathBuf> = Vec::new();
    if let Ok(d) = crate::client::default_state_dir() {
        forbidden.push(d);
    }
    if let Ok(d) = crate::home::local_state_dir() {
        forbidden.push(d);
    }
    if let Ok(d) = crate::issue::home_default_dir() {
        forbidden.push(d);
    }
    for live in forbidden {
        let live = crate::sandbox::resolved(&live);
        if ours.starts_with(&live) || live.starts_with(&ours) {
            return Err(Error::rejected(format!(
                "{} overlaps a production path {} — a staging instance can never be \
                 registered there",
                ours.display(),
                live.display()
            )));
        }
    }
    Ok(())
}

/// The canonical spelling of a state dir — each existing prefix resolved,
/// `..` applied lexically — so a grant bound to one spelling binds the dir.
pub fn canonical_state_dir(state_dir: &Path) -> Result<PathBuf> {
    Ok(crate::sandbox::resolved(state_dir))
}

/// Is `state_dir` registered staging? Read by `staging delegate` before a
/// grant is written — no instance row, no grant (contract I2/I3).
pub(super) fn is_staging(conn: &Connection, canonical: &str) -> Result<bool> {
    if !table_exists(conn, "staging_instances")? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT 1 FROM staging_instances WHERE id=1 AND state_dir=?1",
        [canonical],
        |_| Ok(()),
    )
    .optional()
    .map(|o| o.is_some())
    .map_err(Into::into)
}

/// `staging delegate` (CAD-1024): the operator grants `alias` the listed ops
/// on this staging state dir for `ttl` (≤ 7d). The dir must be registered
/// staging first; `ops` is validated against [`STAGING_OPS`]; the grant is
/// bound to the canonical state dir. A new grant supersedes the alias's live
/// one. Operator-only — the RPC handler gates on `operator_connection`.
pub fn staging_delegate(
    state_dir: &Path,
    alias: &str,
    ops: &[String],
    ttl: Duration,
    by: &str,
) -> Result<Value> {
    validate_identity(alias)?;
    if ttl.is_zero() || ttl > MAX_TTL {
        return Err(Error::rejected("a staging grant's --ttl must be 1s–7d"));
    }
    if ops.is_empty() {
        return Err(Error::rejected("--ops must name at least one op"));
    }
    for op in ops {
        if !STAGING_OPS.contains(&op.as_str()) {
            return Err(Error::rejected(format!(
                "op '{op}' is not in the v1 staging allowlist ({})",
                STAGING_OPS.join(", ")
            )));
        }
    }
    let canonical = canonical_state_dir(state_dir)?;
    let canonical = canonical.to_string_lossy().into_owned();
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    committed(immediate(&conn, |conn| {
        if !is_staging(conn, &canonical)? {
            return Ok(TxResult::Refuse(format!(
                "{canonical} is not a registered staging instance — run `cadence staging register` first"
            )));
        }
        let until = now + ttl.as_secs_f64();
        let ops_json = json!(ops).to_string();
        conn.execute(
            "UPDATE staging_grants SET revoked_at=?1, revoked_by='superseded'
             WHERE alias=?2 AND revoked_at IS NULL",
            params![now, alias],
        )?;
        conn.execute(
            "INSERT INTO staging_grants(alias,ops,state_dir,granted_by,granted_at,expires_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![alias, ops_json, canonical, by, now, until],
        )?;
        let payload = json!({
            "alias": alias, "ops": ops, "state_dir": canonical,
            "by": by, "expires_at": until,
        });
        insert_event(conn, "staging_delegate", payload.clone(), now)?;
        Ok(TxResult::Done(json!({
            "granted": alias, "ops": ops, "state_dir": canonical,
            "by": by, "granted_at": now, "expires_at": until,
        })))
    }))
}

/// `staging revoke` (CAD-1024): end `alias`'s live grant on this state dir.
/// Operator-only, like [`staging_delegate`].
pub fn staging_revoke(state_dir: &Path, alias: &str, by: &str) -> Result<Value> {
    let conn = connect_ensured(&db_file(state_dir))?;
    let now = unix_now();
    committed(immediate(&conn, |conn| {
        let n = conn.execute(
            "UPDATE staging_grants SET revoked_at=?1, revoked_by=?2
             WHERE alias=?3 AND revoked_at IS NULL AND expires_at > ?1",
            params![now, by, alias],
        )?;
        if n == 0 {
            return Ok(TxResult::Refuse(format!(
                "'{alias}' holds no live staging grant"
            )));
        }
        insert_event(
            conn,
            "staging_revoke",
            json!({"alias": alias, "by": by}),
            now,
        )?;
        Ok(TxResult::Done(
            json!({"revoked": alias, "by": by, "at": now}),
        ))
    }))
}

/// `staging delegations` (CAD-1024): the live grants on this state dir.
/// Read-only — the RPC rule is `Read`.
pub fn staging_delegations(state_dir: &Path) -> Result<Value> {
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(json!({"delegations": []}));
    }
    let conn = connect(&path)?;
    let now = unix_now();
    if !table_exists(&conn, "staging_grants")? {
        return Ok(json!({"delegations": []}));
    }
    let mut stmt = conn.prepare(
        "SELECT alias, ops, state_dir, granted_by, granted_at, expires_at, last_used_at
         FROM staging_grants WHERE revoked_at IS NULL AND expires_at > ?1 ORDER BY alias",
    )?;
    let rows = stmt.query_map(params![now], |row| {
        Ok(json!({
            "alias": row.get::<_, String>(0)?,
            "ops": serde_json::from_str::<Value>(&row.get::<_, String>(1)?)
                .unwrap_or(Value::Null),
            "state_dir": row.get::<_, String>(2)?,
            "by": row.get::<_, String>(3)?,
            "granted_at": row.get::<_, f64>(4)?,
            "expires_at": row.get::<_, f64>(5)?,
            "last_used_at": row.get::<_, Option<f64>>(6)?,
        }))
    })?;
    let delegations = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!({"delegations": delegations}))
}

/// The alias a `delegate:<alias>` caller claims, parsed out of the identity.
/// `None` for any other caller shape.
pub fn delegate_alias(caller: &Caller) -> Option<&str> {
    if caller.source == "delegate" {
        caller.identity.strip_prefix("delegate:")
    } else {
        None
    }
}

/// The v1 delegate gate (CAD-1024, contract I3/I4): a `delegate:<alias>`
/// caller may exercise `op` on `state_dir` only when (a) its process natively
/// derives to `alias` and (b) a live `staging_grants` row names `alias`, `op`,
/// and this dir's canonical path. Reads the caller's own env + `/proc`
/// ancestry; every failure is `Err(rejected)`. Returns the bare `alias`.
pub fn require_delegate_grant(state_dir: &Path, caller: &Caller, op: &str) -> Result<String> {
    let env = env_nonempty("CADENCE_ALIAS");
    require_delegate_grant_inner(state_dir, caller, op, env.as_deref(), std::process::id())
}

/// The delegate check with the caller's ambient alias and pid injected (the
/// testable core of [`require_delegate_grant`]).
pub(super) fn require_delegate_grant_inner(
    state_dir: &Path,
    caller: &Caller,
    op: &str,
    env_alias: Option<&str>,
    caller_pid: u32,
) -> Result<String> {
    let Some(alias) = delegate_alias(caller) else {
        return Err(Error::rejected(format!(
            "{op} as a delegate needs `--as delegate:<alias>`"
        )));
    };
    // (a) The native identity binding — the alias is this process's own.
    let env = env_alias.ok_or_else(|| {
        Error::rejected(format!(
            "{op} as delegate:{alias} refused: this process carries no CADENCE_ALIAS"
        ))
    })?;
    if env != alias {
        return Err(Error::rejected(format!(
            "{op} as delegate:{alias} refused: this process's CADENCE_ALIAS is '{env}'"
        )));
    }
    // `alias` must be registered in *this* state dir with a live endpoint on
    // this process's ancestry — the caller's pane or managed root. An alias
    // registered only in another state dir proves nothing here.
    let chain = crate::adapter::pty::caller_chain(caller_pid)
        .ok_or_else(|| Error::rejected(format!("{op}: /proc ancestry unreadable")))?;
    let path = db_file(state_dir);
    let peek = open_peek(&path)?;
    if !table_exists(&peek.conn, "agents")? {
        return Err(Error::rejected(format!(
            "{op} as delegate:{alias} refused: no agents registered here"
        )));
    }
    let has_start = peek
        .conn
        .prepare("SELECT 1 FROM pragma_table_info('agents') WHERE name='pid_start'")?
        .exists([])?;
    let start = if has_start { "pid_start" } else { "NULL" };
    let row: Option<(i64, Option<i64>)> = peek
        .conn
        .query_row(
            &format!("SELECT pid, {start} FROM agents WHERE alias=?1 AND pid IS NOT NULL"),
            [alias],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((pid, start)) = row else {
        return Err(Error::rejected(format!(
            "{op} as delegate:{alias} refused: '{alias}' is not registered in this state dir"
        )));
    };
    let pid = u32::try_from(pid)
        .ok()
        .filter(|p| chain.contains(p))
        .ok_or_else(|| {
            Error::rejected(format!(
                "{op} as delegate:{alias} refused: its registered endpoint is not on this \
                 process's ancestry (a forged CADENCE_ALIAS on an unregistered process derives \
                 nothing)"
            ))
        })?;
    // pid_start fencing: a reused pid denies nothing.
    let proven = crate::peer::AgentPids::classify([(
        alias.to_string(),
        pid,
        start.and_then(|s| u64::try_from(s).ok()),
    )]);
    if proven.fenced().is_empty() {
        return Err(Error::rejected(format!(
            "{op} as delegate:{alias} refused: its recorded pid {pid} was reused"
        )));
    }
    // (b) The live grant for (alias, op, this dir).
    let canonical = canonical_state_dir(state_dir)?
        .to_string_lossy()
        .into_owned();
    let now = unix_now();
    let live: Option<i64> = peek
        .conn
        .query_row(
            "SELECT id FROM staging_grants
             WHERE alias=?1 AND state_dir=?2 AND revoked_at IS NULL AND expires_at > ?3
             ORDER BY id DESC LIMIT 1",
            params![alias, canonical, now],
            |r| r.get(0),
        )
        .optional()?;
    let Some(grant_id) = live else {
        return Err(Error::rejected(format!(
            "{op} as delegate:{alias} refused: no live grant for '{alias}' on {canonical}"
        )));
    };
    let covers: bool = peek.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM staging_grants g, json_each(g.ops) o
              WHERE g.id=?1 AND o.value=?2)",
        params![grant_id, op],
        |r| r.get(0),
    )?;
    if !covers {
        return Err(Error::rejected(format!(
            "{op} as delegate:{alias} refused: '{op}' is outside its grant"
        )));
    }
    Ok(alias.to_string())
}

pub fn note_restart_proceeded(state_dir: &Path, ticket: &RestartTicket) -> Result<()> {
    let conn = connect(&db_file(state_dir))?;
    immediate(&conn, |tx| {
        insert_event(
            tx,
            "rollout_restart_proceeded",
            json!({
                "holder": ticket.holder,
                "lease_id": ticket.lease_id,
                "build": crate::overview::BUILD_COMMIT,
            }),
            unix_now(),
        )
    })
}
