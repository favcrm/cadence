//! Rollout caller: see the parent module.

use super::*;

thread_local! {
    /// Test override so a bootstrap opt-in does not leak across threads
    /// through the process environment.
    pub(super) static BOOTSTRAP_FORCE: Cell<Option<bool>> = const { Cell::new(None) };
    /// Test override for the identity allowed to migrate. Unset in
    /// production; the daemon reads `CADENCE_ROLLOUT_AS` then
    /// `CADENCE_ALIAS`.
    pub(super) static MIGRATION_HOLDER: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Identity passed on `daemon run --rollout-as`. Process-wide, not
/// thread-local: `new_hot` must see it even if a later refactor moves
/// that call off the main thread. Set once, from `main`, before any
/// worker starts. Kept out of the environment so panes do not inherit it.
pub(super) static FORWARDED_AS: OnceLock<String> = OnceLock::new();

/// Remember the holder forwarded by `daemon start` for this process.
pub fn set_forwarded_identity(identity: Option<String>) {
    remember_forwarded(&FORWARDED_AS, identity);
}

/// Set `slot` once. Tests pass a local lock so a run of
/// `cargo test --lib rollout` does not stick a process-wide identity
/// that later tests cannot clear.
pub(super) fn remember_forwarded(slot: &OnceLock<String>, identity: Option<String>) {
    let Some(identity) = identity
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let _ = slot.set(identity);
}

pub(super) fn forwarded_identity() -> Option<String> {
    read_forwarded(&FORWARDED_AS)
}

pub(super) fn read_forwarded(slot: &OnceLock<String>) -> Option<String> {
    slot.get().cloned()
}

pub(super) fn bootstrap_requested() -> bool {
    if let Some(forced) = BOOTSTRAP_FORCE.with(Cell::get) {
        return forced;
    }
    std::env::var("CADENCE_ROLLOUT_BOOTSTRAP").ok().as_deref() == Some("1")
}

/// Identity allowed to migrate: the test override, else the identity
/// forwarded into this process, else the pane alias.
pub(super) fn caller_for_migration() -> Option<String> {
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
pub(super) fn process_caller() -> Result<Caller> {
    if let Some(identity) = forwarded_identity() {
        validate_identity(&identity)?;
        return Ok(Caller {
            identity,
            source: "as",
        });
    }
    resolve_caller(None)
}

pub(super) fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
pub(super) const MAX_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub(super) const FORCE_RELEASE_HINT: &str = "An expired holder is cleared with \
     `cadence rollout release --force --reason \"<why>\" --as operator:<name>`. \
     A lease that has not expired also requires `--holder <that identity>`.";
pub(super) const GATE_LOG: &str = "rollout-gate.jsonl";

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
pub(super) enum TxResult<T> {
    Done(T),
    Refuse(String),
}

pub(super) fn committed<T>(result: Result<TxResult<T>>) -> Result<T> {
    match result {
        Ok(TxResult::Done(value)) => Ok(value),
        Ok(TxResult::Refuse(message)) => Err(Error::rejected(message)),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone)]
pub(super) struct Lease {
    pub(super) id: i64,
    pub(super) holder: String,
    pub(super) holder_source: String,
    pub(super) host_note: String,
    pub(super) reason: String,
    pub(super) target_commit: Option<String>,
    pub(super) schema_from: Option<i64>,
    pub(super) schema_to: Option<i64>,
    pub(super) backup_path: Option<String>,
    pub(super) backup_sha256: Option<String>,
    pub(super) backup_schema: Option<i64>,
    pub(super) backup_taken_at: Option<f64>,
    pub(super) claimed_at: f64,
    pub(super) expires_at: f64,
    pub(super) status: String,
    pub(super) previous_holder: Option<String>,
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
    // CAD-1024: a `delegate:<alias>` `--as` is owned by the env alias it
    // names — `delegate:w1` and `CADENCE_ALIAS=w1` are the same caller.
    let explicit_matches_alias = |alias: &str, explicit: &str| {
        alias == explicit
            || explicit
                .strip_prefix("delegate:")
                .is_some_and(|d| d == alias)
    };
    match (alias, explicit) {
        (Some(alias), Some(explicit)) if !explicit_matches_alias(alias, explicit) => {
            Err(Error::rejected(
                "inside a cadence pane the lease holder is $CADENCE_ALIAS; \
                 --as must match it. Ownership is never taken from a path or HEAD",
            ))
        }
        (Some(alias), explicit) => {
            // A `delegate:` explicit that matched this alias carries the
            // delegate shape (the lease holder is the literal `delegate:<a>`);
            // a matching non-delegate explicit or none keeps the alias caller.
            let delegate = explicit.and_then(|e| e.strip_prefix("delegate:"));
            if let Some(d) = delegate {
                if d.is_empty() || d.starts_with("operator") {
                    return Err(Error::rejected(
                        "--as delegate:<alias> needs the agent's alias, never operator:*",
                    ));
                }
                return Ok(Caller {
                    identity: explicit.unwrap().to_string(),
                    source: "delegate",
                });
            }
            validate_identity(alias)?;
            Ok(Caller {
                identity: alias.to_string(),
                source: "alias",
            })
        }
        (None, Some(explicit)) => {
            validate_identity(explicit)?;
            // CAD-1024: `delegate:<alias>` is a third caller shape — the
            // literal `delegate:<alias>` is the lease holder, and `op`
            // admission is gated by `require_delegate_grant`, never by
            // `operator_proof` (which a delegate must not pass and must not
            // need). The bare `alias` is recovered by [`delegate_alias`].
            if let Some(alias) = explicit.strip_prefix("delegate:") {
                if alias.is_empty() || alias.starts_with("operator") {
                    return Err(Error::rejected(
                        "--as delegate:<alias> needs the agent's alias, never operator:*",
                    ));
                }
                return Ok(Caller {
                    identity: explicit.to_string(),
                    source: "delegate",
                });
            }
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

pub(super) fn validate_identity(identity: &str) -> Result<()> {
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
