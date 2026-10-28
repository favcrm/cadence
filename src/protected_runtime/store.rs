//! The private owner authority SQLite.
//!
//! Location: `<state_dir>/runtime-owner.sqlite`, created inside a `0700`
//! root-owned directory with a `0600` file. This database is *authority*, not a
//! secret: it holds the owner epoch, runtime obligations and their append-only
//! transitions, UID allocations, peer enrollments, the external watermark and
//! the restore log. It never holds credentials, keys or plaintext.
//!
//! Rules encoded here:
//!
//! - checked monotonic `owner_epoch`/`generation` increments refuse overflow;
//! - a phase transition is one `UPDATE ... WHERE phase = ?` CAS inside one
//!   transaction that also appends the transition; zero changed rows refuses;
//! - history is append-only (triggers abort UPDATE/DELETE);
//! - a UID/name/generation is never reused, including after revocation;
//! - the external checkpoint watermark only moves forward: an equal sequence
//!   with a different digest refuses, a stale sequence cannot lower it;
//! - restoring a snapshot does not restore admission: `restore_begin` fences
//!   the epoch and `restore_complete` requires a strictly newer epoch.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};

use super::profile;

const SCHEMA_VERSION: i64 = 2;
const DB_FILE: &str = "runtime-owner.sqlite";

pub(crate) const PHASE_ALLOCATING: &str = "allocating";
pub(crate) const PHASE_PREPARED: &str = "prepared";
pub(crate) const PHASE_CONSUMED: &str = "consumed";
pub(crate) const PHASE_STARTING: &str = "starting";
pub(crate) const PHASE_OBSERVING: &str = "observing";
pub(crate) const PHASE_READY: &str = "ready";
pub(crate) const PHASE_UNKNOWN: &str = "unknown";
pub(crate) const PHASE_REVOKING: &str = "revoking";
pub(crate) const PHASE_REVOKED: &str = "revoked";

/// Non-executable phases: no admission, no start, no observation authority.
pub(crate) fn executable(phase: &str) -> bool {
    phase == PHASE_READY
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn db<T>(r: rusqlite::Result<T>) -> Result<T> {
    r.map_err(|e| Error::internal(format!("owner authority database: {e}")))
}

fn conflicts(message: impl Into<String>) -> Error {
    Error::conflict_coded("owner_conflict", message)
}

/// The external prestart witness. Private fields; produced only from the
/// owner's own authority rows.
#[derive(Clone, Debug)]
pub(crate) struct PrestartEligibility {
    pub(crate) workspace_id: String,
    pub(crate) owner_epoch: u64,
    pub(crate) database_epoch: u64,
    pub(crate) restore_state: String,
    pub(crate) min_sequence: u64,
    pub(crate) min_digest: String,
}

/// One runtime obligation row.
#[derive(Clone, Debug)]
pub(crate) struct Obligation {
    pub(crate) runtime_id: String,
    pub(crate) operation_id: String,
    pub(crate) generation: u64,
    /// `None` while allocating: the canonical challenge does not exist until
    /// `enroll_pending` binds it to the already-allocated launch request.
    pub(crate) challenge_json: Option<String>,
    pub(crate) challenge_digest: Option<String>,
    /// The exact canonical LaunchRequest this allocation intends to launch,
    /// its domain digest, the measured supervisor recipient and the actual
    /// owner-launched/enrolled pinned installer. All `None` only before
    /// `set_launch_intent` commits.
    pub(crate) launch_request_json: Option<String>,
    pub(crate) launch_request_digest: Option<String>,
    pub(crate) recipient_pid: Option<u32>,
    pub(crate) recipient_starttime: Option<u64>,
    pub(crate) recipient_generation: Option<String>,
    pub(crate) recipient_nonce: Option<String>,
    pub(crate) installer_pid: Option<u32>,
    pub(crate) installer_starttime: Option<u64>,
    pub(crate) installer_generation: Option<String>,
    pub(crate) profile_digest: String,
    pub(crate) owner_epoch: u64,
    pub(crate) phase: String,
    pub(crate) agent_uid: String,
    pub(crate) session_binding: String,
    pub(crate) server_generation: u64,
    pub(crate) os_uid: u32,
    pub(crate) os_gid: u32,
    pub(crate) container_name: String,
    pub(crate) container_id: Option<String>,
    pub(crate) ops_socket: String,
    pub(crate) attest_socket: String,
    pub(crate) observation_counter: u64,
    pub(crate) observed_registry_digest: Option<String>,
    pub(crate) observed_registry_json: Option<String>,
    pub(crate) loaded_receipt_id: Option<String>,
    pub(crate) node_pid: Option<u32>,
    pub(crate) node_starttime: Option<u64>,
    pub(crate) qualified_at_ms: Option<u64>,
    pub(crate) fenced_reason: Option<String>,
    pub(crate) created_at_ms: u64,
    pub(crate) updated_at_ms: u64,
}

impl Obligation {
    pub(crate) fn uid(&self) -> u32 {
        self.os_uid
    }
    pub(crate) fn gid(&self) -> u32 {
        self.os_gid
    }
    pub(crate) fn executable(&self) -> bool {
        executable(&self.phase)
    }
}

/// Parameters for one allocation. The supervisor supplies the stable agent
/// identity/session; the owner generates runtime id, generation, UID, name and
/// the canonical challenge.
pub(crate) struct AllocParams<'a> {
    pub(crate) workspace_id: &'a str,
    pub(crate) agent_uid: &'a str,
    pub(crate) session_binding: &'a str,
    pub(crate) server_generation: u64,
    pub(crate) expected_owner_epoch: u64,
    pub(crate) database_epoch: u64,
    pub(crate) profile_digest: &'a str,
    pub(crate) max_runtimes: u32,
    pub(crate) uid_start: u32,
    pub(crate) uid_end: u32,
    pub(crate) run_dir: &'a Path,
}

/// A freshly allocated runtime — before the challenge exists.
pub(crate) struct Allocation {
    pub(crate) runtime_id: String,
    /// The immutable owned nonce (never reused) used in container labels.
    pub(crate) operation_id: String,
    pub(crate) generation: u64,
    pub(crate) os_uid: u32,
    pub(crate) os_gid: u32,
    pub(crate) owner_epoch: u64,
    pub(crate) container_name: String,
    pub(crate) ops_socket: String,
    pub(crate) attest_socket: String,
    pub(crate) created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsumeOutcome {
    Consumed,
    Unknown,
}

use cadence_core::connection_dispatch::ledger::LedgerError;
/// The owner checkpoint the daemon sends after a NEW local commitment — never
/// before, never a substitute for admission. The wire/command types are the
/// shared `cadence-core` ledger contract; the owner wraps them with its own
/// runtime/epoch binding and persisted `LedgerWatermarkV1`. There is no second
/// budget or chain implementation here.
pub(crate) use cadence_core::connection_dispatch::ledger::{
    CheckpointAckV1 as CheckpointAck, CheckpointV1, LedgerRecordV1, LedgerWatermarkV1,
};
use cadence_core::connection_dispatch::primitives::{Counter, Digest, Revision};

pub(crate) struct OwnerStore {
    conn: Mutex<Connection>,
    path: PathBuf,
    workspace_id: String,
    boot_id: String,
}

impl OwnerStore {
    /// Open (or create) the authority database in `state_dir`. The directory is
    /// `0700` root-owned and the database `0600`; the owner epoch advances on
    /// every open (an owner restart fences old admission).
    pub(crate) fn open(
        state_dir: &Path,
        workspace_id: &str,
        database_epoch: u64,
        boot_id: &str,
    ) -> Result<Self> {
        ensure_private_dir(state_dir)?;
        let path = state_dir.join(DB_FILE);
        let conn = db(Connection::open(&path))?;
        db(conn.execute_batch(
            "PRAGMA journal_mode = WAL;\
             PRAGMA synchronous = FULL;\
             PRAGMA foreign_keys = ON;\
             PRAGMA trusted_schema = OFF;\
             PRAGMA busy_timeout = 5000;",
        ))?;
        let version: i64 = db(conn.query_row("PRAGMA user_version", [], |r| r.get(0)))?;
        if version == 0 {
            db(conn.execute_batch(SCHEMA_V1))?;
            db(conn.pragma_update(None, "user_version", SCHEMA_VERSION))?;
        } else if version != SCHEMA_VERSION {
            return Err(Error::rejected(format!(
                "owner authority schema version {version} is not supported"
            )));
        }
        set_file_mode(&path, 0o600)?;
        let now = now_ms();
        let store = Self {
            conn: Mutex::new(conn),
            path,
            workspace_id: workspace_id.to_string(),
            boot_id: boot_id.to_string(),
        };
        store.begin_boot(database_epoch, now)?;
        Ok(store)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn begin_boot(&self, database_epoch: u64, now: u64) -> Result<()> {
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("owner authority transaction: {e}")))?;
        let existing: Option<(i64, i64)> = tx
            .query_row(
                "SELECT owner_epoch, database_epoch FROM owner_meta WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| Error::internal(format!("owner_meta read: {e}")))?;
        match existing {
            None => {
                tx.execute(
                    "INSERT INTO owner_meta (id, workspace_id, owner_epoch, boot_id, started_at_ms, \
                     database_epoch, restore_state, created_at_ms) \
                     VALUES (1, ?1, 1, ?2, ?3, ?4, 'stable', ?3)",
                    params![self.workspace_id, self.boot_id, now as i64, database_epoch as i64],
                )
                .map_err(|e| Error::internal(format!("owner_meta insert: {e}")))?;
                let revision = Revision::new(database_epoch)
                    .map_err(|_| Error::rejected("database epoch is out of range"))?;
                let watermark = LedgerWatermarkV1::new(revision);
                persist_watermark(&tx, &watermark)?;
            }
            Some((epoch, db_epoch)) => {
                if db_epoch as u64 != database_epoch {
                    return Err(Error::rejected(
                        "database_epoch differs from the owner's recorded epoch",
                    ));
                }
                let next = epoch as u64 + 1;
                if next > super::MAX_SAFE_INT {
                    return Err(Error::rejected(
                        "owner epoch exhausted — refusing to reuse an epoch",
                    ));
                }
                // Restart advances the epoch before serving and fences every
                // non-revoked runtime: authentic old snapshots/grants cannot be
                // replayed into admission. Each transition records the phase
                // that was actually current.
                fence_every_runtime(&tx, "owner restart", now)?;
                tx.execute(
                    "UPDATE owner_meta SET owner_epoch = ?1, boot_id = ?2, started_at_ms = ?3 WHERE id = 1",
                    params![next as i64, self.boot_id, now as i64],
                )
                .map_err(|e| Error::internal(format!("owner_meta epoch: {e}")))?;
            }
        }
        tx.commit()
            .map_err(|e| Error::internal(format!("owner authority commit: {e}")))?;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::internal("owner authority mutex poisoned"))
    }

    pub(crate) fn owner_epoch(&self) -> Result<u64> {
        let guard = self.lock()?;
        let epoch: i64 = db(guard.query_row(
            "SELECT owner_epoch FROM owner_meta WHERE id = 1",
            [],
            |r| r.get(0),
        ))?;
        Ok(epoch as u64)
    }

    pub(crate) fn database_epoch(&self) -> Result<u64> {
        let guard = self.lock()?;
        let epoch: i64 = db(guard.query_row(
            "SELECT database_epoch FROM owner_meta WHERE id = 1",
            [],
            |r| r.get(0),
        ))?;
        Ok(epoch as u64)
    }

    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// The authenticated external prestart witness: the owner's own
    /// workspace/epochs/restore state plus its retained ledger watermark
    /// floor. Read-only; it never mints or advances anything. This is the
    /// root physical issuer's provenance document a protected store open may
    /// validate against (never a caller field, never `ever_enabled` alone).
    pub(crate) fn prestart_eligibility(&self) -> Result<PrestartEligibility> {
        let guard = self.lock()?;
        let (workspace_id, owner_epoch, database_epoch, restore_state): (String, i64, i64, String) =
            db(guard.query_row(
                "SELECT workspace_id, owner_epoch, database_epoch, restore_state \
                 FROM owner_meta WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            ))?;
        let (min_sequence, min_digest): (String, String) = db(guard.query_row(
            "SELECT minimum_sequence, minimum_digest FROM owner_watermark WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ))?;
        let min_sequence = min_sequence
            .parse::<u64>()
            .map_err(|_| Error::internal("owner watermark sequence is malformed"))?;
        Ok(PrestartEligibility {
            workspace_id,
            owner_epoch: owner_epoch as u64,
            database_epoch: database_epoch as u64,
            restore_state,
            min_sequence,
            min_digest,
        })
    }

    /// Allocate one runtime instance under the current epoch. Checked,
    /// monotonic, never reusing a UID, name or generation.
    pub(crate) fn allocate(&self, p: AllocParams<'_>) -> Result<Allocation> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("allocate transaction: {e}")))?;
        let (epoch, db_epoch, state): (i64, i64, String) = tx
            .query_row(
                "SELECT owner_epoch, database_epoch, restore_state FROM owner_meta WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| Error::internal(format!("owner_meta read: {e}")))?;
        if state != "stable" {
            return Err(Error::rejected(
                "owner is restoring/fenced — no new runtime admission",
            ));
        }
        if epoch as u64 != p.expected_owner_epoch {
            return Err(conflicts(format!(
                "expected owner epoch {} does not match the current epoch {}",
                p.expected_owner_epoch, epoch
            )));
        }
        if db_epoch as u64 != p.database_epoch {
            // A profile naming a different database epoch is a lineage
            // mismatch; the owner never reinterprets a foreign encrypted DB.
            return Err(Error::rejected(
                "profile database epoch differs from the owner authority epoch",
            ));
        }
        let live: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM runtime_obligations WHERE phase NOT IN ('revoked','unknown')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Error::internal(format!("live count: {e}")))?;
        if live as u32 >= p.max_runtimes {
            return Err(Error::rejected("runtime limit reached"));
        }
        // UID: first free value in the configured range, never reused.
        let mut uid = None;
        let mut candidate = p.uid_start;
        while candidate <= p.uid_end {
            let used: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM uid_allocations WHERE os_uid = ?1",
                    params![candidate as i64],
                    |r| r.get(0),
                )
                .map_err(|e| Error::internal(format!("uid allocation read: {e}")))?;
            if used == 0 {
                uid = Some(candidate);
                break;
            }
            candidate += 1;
        }
        let os_uid = uid.ok_or_else(|| Error::rejected("runtime uid range exhausted"))?;
        // Generation: monotonic across every runtime this owner ever created.
        let last_gen: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(generation), 0) FROM runtime_obligations",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Error::internal(format!("generation read: {e}")))?;
        let generation = last_gen as u64 + 1;
        if generation > super::MAX_SAFE_INT {
            return Err(Error::rejected(
                "runtime generation exhausted — refusing to reuse a generation",
            ));
        }
        let runtime_id = uuid::Uuid::new_v4().to_string();
        let short = runtime_id.replace('-', "");
        let container_name = format!("cadence-protected-{}", &short[..16]);
        let ops_socket = p
            .run_dir
            .join("runtimes")
            .join(&runtime_id)
            .join("ops.sock");
        let attest_socket = p
            .run_dir
            .join("runtimes")
            .join(&runtime_id)
            .join("attest.sock");
        let ops_socket = ops_socket
            .to_str()
            .ok_or_else(|| Error::internal("ops socket path is not UTF-8"))?
            .to_string();
        let attest_socket = attest_socket
            .to_str()
            .ok_or_else(|| Error::internal("attest socket path is not UTF-8"))?
            .to_string();
        let operation_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO runtime_obligations (runtime_id, operation_id, generation, profile_digest, \
             owner_epoch, phase, agent_uid, session_binding, server_generation, os_uid, os_gid, \
             container_name, ops_socket, attest_socket, created_at_ms, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'allocating', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
            params![
                runtime_id,
                operation_id,
                generation as i64,
                p.profile_digest,
                epoch,
                p.agent_uid,
                p.session_binding,
                p.server_generation as i64,
                os_uid as i64,
                os_uid as i64,
                container_name,
                ops_socket,
                attest_socket,
                now as i64
            ],
        )
        .map_err(|e| Error::internal(format!("obligation insert: {e}")))?;
        tx.execute(
            "INSERT INTO uid_allocations (os_uid, runtime_id) VALUES (?1, ?2)",
            params![os_uid as i64, runtime_id],
        )
        .map_err(|e| Error::internal(format!("uid allocation insert: {e}")))?;
        tx.execute(
            "INSERT INTO obligation_transitions (runtime_id, from_phase, to_phase, at_ms) \
             VALUES (?1, NULL, 'allocating', ?2)",
            params![runtime_id, now as i64],
        )
        .map_err(|e| Error::internal(format!("transition insert: {e}")))?;
        tx.commit()
            .map_err(|e| Error::internal(format!("allocate commit: {e}")))?;
        Ok(Allocation {
            runtime_id,
            operation_id,
            generation,
            os_uid,
            os_gid: os_uid,
            owner_epoch: epoch as u64,
            container_name,
            ops_socket,
            attest_socket,
            created_at_ms: now,
        })
    }

    /// The exact owner-committed launch intent: canonical LaunchRequest bytes
    /// and digest, the measured supervisor recipient, and the actual
    /// owner-launched pinned installer. Committed while `allocating` and before
    /// any challenge handshake; a second commit for the same allocation refuses.
    #[allow(clippy::too_many_arguments)] // the launch intent mirrors the frozen owner schema
    pub(crate) fn set_launch_intent(
        &self,
        runtime_id: &str,
        launch_request_json: &str,
        launch_request_digest: &str,
        recipient_pid: u32,
        recipient_starttime: u64,
        recipient_generation: &str,
        recipient_nonce: &str,
        installer_pid: u32,
        installer_starttime: u64,
        installer_generation: &str,
    ) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("launch intent transaction: {e}")))?;
        let changed = tx
            .execute(
                "UPDATE runtime_obligations SET launch_request_json = ?2, launch_request_digest = ?3, \
                 recipient_pid = ?4, recipient_starttime = ?5, recipient_generation = ?6, \
                 recipient_nonce = ?7, installer_pid = ?8, installer_starttime = ?9, \
                 installer_generation = ?10, updated_at_ms = ?11 \
                 WHERE runtime_id = ?1 AND phase = 'allocating' AND launch_request_digest IS NULL",
                params![
                    runtime_id,
                    launch_request_json,
                    launch_request_digest,
                    recipient_pid as i64,
                    recipient_starttime as i64,
                    recipient_generation,
                    recipient_nonce,
                    installer_pid as i64,
                    installer_starttime as i64,
                    installer_generation,
                    now as i64
                ],
            )
            .map_err(|e| Error::internal(format!("launch intent update: {e}")))?;
        if changed != 1 {
            return Err(conflicts(
                "runtime is not an allocating intent with an uncommitted launch request",
            ));
        }
        tx.commit()
            .map_err(|e| Error::internal(format!("launch intent commit: {e}")))?;
        Ok(())
    }

    pub(crate) fn set_challenge(
        &self,
        runtime_id: &str,
        challenge_json: &str,
        challenge_digest: &str,
    ) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("challenge transaction: {e}")))?;
        let changed = tx
            .execute(
                "UPDATE runtime_obligations SET challenge_json = ?2, challenge_digest = ?3, \
                 updated_at_ms = ?4 WHERE runtime_id = ?1 AND phase = 'allocating' \
                 AND challenge_digest IS NULL",
                params![runtime_id, challenge_json, challenge_digest, now as i64],
            )
            .map_err(|e| Error::internal(format!("challenge update: {e}")))?;
        if changed != 1 {
            return Err(conflicts("runtime is not allocating"));
        }
        tx.commit()
            .map_err(|e| Error::internal(format!("challenge commit: {e}")))?;
        Ok(())
    }

    pub(crate) fn record_container(&self, runtime_id: &str, container_id: &str) -> Result<()> {
        let now = now_ms();
        let guard = self.lock()?;
        let changed = db(guard.execute(
            "UPDATE runtime_obligations SET container_id = ?2, updated_at_ms = ?3 \
             WHERE runtime_id = ?1 AND container_id IS NULL",
            params![runtime_id, container_id, now as i64],
        ))?;
        if changed != 1 {
            return Err(conflicts(
                "container id was already recorded for this runtime",
            ));
        }
        Ok(())
    }

    /// `allocating -> prepared` when the installer durably enrolls the exact
    /// challenge. The digest must equal the stored canonical challenge.
    pub(crate) fn mark_prepared(&self, runtime_id: &str, challenge_digest: &str) -> Result<()> {
        self.cas(
            runtime_id,
            PHASE_ALLOCATING,
            PHASE_PREPARED,
            Some(challenge_digest),
        )
    }

    pub(crate) fn get(&self, runtime_id: &str) -> Result<Obligation> {
        let guard = self.lock()?;
        guard
            .query_row(
                &format!(
                    "SELECT {OBLIGATION_COLUMNS} FROM runtime_obligations WHERE runtime_id = ?1"
                ),
                params![runtime_id],
                row_to_obligation,
            )
            .optional()
            .map_err(|e| Error::internal(format!("obligation read: {e}")))?
            .ok_or_else(|| Error::rejected("runtime is not known to the owner"))
    }

    pub(crate) fn by_challenge(&self, digest: &str) -> Result<Option<Obligation>> {
        let guard = self.lock()?;
        guard
            .query_row(
                &format!(
                    "SELECT {OBLIGATION_COLUMNS} FROM runtime_obligations WHERE challenge_digest = ?1"
                ),
                params![digest],
                row_to_obligation,
            )
            .optional()
            .map_err(|e| Error::internal(format!("obligation read: {e}")))
    }

    /// `enrolled` — true only for the exact durable prepared obligation and
    /// exact enrolled recipient. Absent/unknown/consumed is refusal.
    pub(crate) fn enrolled(&self, digest: &str, owner_epoch: u64) -> Result<bool> {
        match self.by_challenge(digest)? {
            Some(o) => Ok(o.phase == PHASE_PREPARED && o.owner_epoch == owner_epoch),
            None => Ok(false),
        }
    }

    /// Read-only grant continuity before launch, including positive consumption.
    /// Enrollment itself remains prepared-only; this never consumes or starts.
    pub(crate) fn recheck_grant(&self, digest: &str, owner_epoch: u64) -> Result<bool> {
        let guard = self.lock()?;
        db(guard.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_obligations \
             WHERE challenge_digest = ?1 AND owner_epoch = ?2 \
             AND owner_epoch = (SELECT owner_epoch FROM owner_meta WHERE id = 1) \
             AND phase IN ('prepared', 'consumed'))",
            params![digest, owner_epoch],
            |row| row.get(0),
        ))
    }

    /// `prepared -> consumed` addressed by the exact canonical challenge
    /// digest; an absent/ambiguous obligation is `Unknown`.
    pub(crate) fn consume_by_challenge(&self, digest: &str) -> Result<ConsumeOutcome> {
        let Some(row) = self.by_challenge(digest)? else {
            return Ok(ConsumeOutcome::Unknown);
        };
        self.consume(&row.runtime_id, digest)
    }

    /// One-time `prepared -> consumed`. A lost/ambiguous acknowledgement is
    /// `Unknown`, never retried as success.
    pub(crate) fn consume(&self, runtime_id: &str, digest: &str) -> Result<ConsumeOutcome> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("consume transaction: {e}")))?;
        let current: Option<(String, Option<String>, i64)> = tx
            .query_row(
                "SELECT phase, challenge_digest, owner_epoch FROM runtime_obligations WHERE runtime_id = ?1",
                params![runtime_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| Error::internal(format!("consume read: {e}")))?;
        let epoch: i64 = tx
            .query_row("SELECT owner_epoch FROM owner_meta WHERE id = 1", [], |r| {
                r.get(0)
            })
            .map_err(|e| Error::internal(format!("epoch read: {e}")))?;
        let Some((phase, stored, recorded_epoch)) = current else {
            return Ok(ConsumeOutcome::Unknown);
        };
        if phase != PHASE_PREPARED || stored.as_deref() != Some(digest) || recorded_epoch != epoch {
            // Replay, stale epoch, or a different challenge: uncertain/absent
            // is never success.
            return Ok(ConsumeOutcome::Unknown);
        }
        let changed = tx
            .execute(
                "UPDATE runtime_obligations SET phase = 'consumed', updated_at_ms = ?2 \
                 WHERE runtime_id = ?1 AND phase = 'prepared' AND challenge_digest = ?3",
                params![runtime_id, now as i64, digest],
            )
            .map_err(|e| Error::internal(format!("consume update: {e}")))?;
        if changed != 1 {
            return Ok(ConsumeOutcome::Unknown);
        }
        append_transition(&tx, runtime_id, PHASE_PREPARED, PHASE_CONSUMED, now)?;
        tx.commit()
            .map_err(|e| Error::internal(format!("consume commit: {e}")))?;
        Ok(ConsumeOutcome::Consumed)
    }

    /// `consumed -> starting`; only the exact consumed obligation may start.
    pub(crate) fn mark_starting(&self, runtime_id: &str, generation: u64) -> Result<Obligation> {
        self.cas_generation(runtime_id, PHASE_CONSUMED, PHASE_STARTING, generation)?;
        self.get(runtime_id)
    }

    pub(crate) fn mark_observing(
        &self,
        runtime_id: &str,
        generation: u64,
        key: &[u8; 32],
        node_pid: u32,
        node_starttime: u64,
    ) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("observing transaction: {e}")))?;
        let changed = tx
            .execute(
                "UPDATE runtime_obligations SET phase = 'observing', observation_key = ?2, \
                 node_pid = ?3, node_starttime = ?4, updated_at_ms = ?5 \
                 WHERE runtime_id = ?1 AND phase = 'starting' AND generation = ?6",
                params![
                    runtime_id,
                    key.as_slice(),
                    node_pid as i64,
                    node_starttime as i64,
                    now as i64,
                    generation as i64
                ],
            )
            .map_err(|e| Error::internal(format!("observing update: {e}")))?;
        if changed != 1 {
            return Err(conflicts("runtime is not starting"));
        }
        append_transition(&tx, runtime_id, PHASE_STARTING, PHASE_OBSERVING, now)?;
        tx.commit()
            .map_err(|e| Error::internal(format!("observing commit: {e}")))?;
        Ok(())
    }

    /// Record one verified observation; only the current generation, an
    /// increasing counter and the exact observation key are accepted.
    #[allow(clippy::too_many_arguments)] // the observation mirrors the frozen owner schema
    pub(crate) fn record_observation(
        &self,
        runtime_id: &str,
        generation: u64,
        key: &[u8; 32],
        counter: u64,
        registry_digest: &str,
        registry_json: &str,
        loaded_receipt_id: &str,
    ) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("observation transaction: {e}")))?;
        let current: Option<(String, i64, Vec<u8>, i64)> = tx
            .query_row(
                "SELECT phase, generation, observation_key, observation_counter FROM runtime_obligations \
                 WHERE runtime_id = ?1",
                params![runtime_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| Error::internal(format!("observation read: {e}")))?;
        let Some((phase, gen, stored_key, stored_counter)) = current else {
            return Err(Error::rejected("runtime is not known to the owner"));
        };
        if phase != PHASE_OBSERVING || gen as u64 != generation {
            return Err(Error::rejected(
                "runtime is not observing at this generation",
            ));
        }
        if stored_key.as_slice() != key.as_slice() {
            return Err(Error::rejected("observation key mismatch"));
        }
        if counter <= stored_counter as u64 {
            return Err(Error::rejected(
                "observation counter must strictly increase",
            ));
        }
        tx.execute(
            "UPDATE runtime_obligations SET observation_counter = ?2, observed_registry_digest = ?3, \
             observed_registry_json = ?4, loaded_receipt_id = ?5, updated_at_ms = ?6 \
             WHERE runtime_id = ?1",
            params![
                runtime_id,
                counter as i64,
                registry_digest,
                registry_json,
                loaded_receipt_id,
                now as i64
            ],
        )
        .map_err(|e| Error::internal(format!("observation update: {e}")))?;
        tx.commit()
            .map_err(|e| Error::internal(format!("observation commit: {e}")))?;
        Ok(())
    }

    /// `observing -> ready` after the live measurement and the observed
    /// registry both matched. The caller must hold the non-forgeable
    /// `VerifiedRegistry` proof produced by `registry::verify_payload` (which
    /// already checked the stable projection against the profile pin); the
    /// stored full dynamic digest must equal the proof's dynamic digest.
    pub(crate) fn mark_ready(
        &self,
        runtime_id: &str,
        generation: u64,
        verified: &super::registry::VerifiedRegistry,
    ) -> Result<()> {
        if verified.generation() != generation {
            return Err(Error::rejected(
                "verified registry names a different runtime generation",
            ));
        }
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("ready transaction: {e}")))?;
        let changed = tx
            .execute(
                "UPDATE runtime_obligations SET phase = 'ready', qualified_at_ms = ?2, \
                 updated_at_ms = ?2 WHERE runtime_id = ?1 AND phase = 'observing' \
                 AND generation = ?3 AND observed_registry_digest = ?4",
                params![
                    runtime_id,
                    now as i64,
                    generation as i64,
                    verified.dynamic_digest()
                ],
            )
            .map_err(|e| Error::internal(format!("ready update: {e}")))?;
        if changed != 1 {
            return Err(conflicts(
                "runtime cannot become ready from its current phase/registry",
            ));
        }
        append_transition(&tx, runtime_id, PHASE_OBSERVING, PHASE_READY, now)?;
        tx.commit()
            .map_err(|e| Error::internal(format!("ready commit: {e}")))?;
        Ok(())
    }

    /// Fence: every non-revoked phase may be fenced; only a matched expected
    /// generation. Ready/observing/starting/consumed/prepared/allocating are
    /// all stopped. The row is retained forever.
    pub(crate) fn fence(
        &self,
        runtime_id: &str,
        expected_generation: u64,
        reason: &str,
    ) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("fence transaction: {e}")))?;
        let current: Option<(String, i64)> = tx
            .query_row(
                "SELECT phase, generation FROM runtime_obligations WHERE runtime_id = ?1",
                params![runtime_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| Error::internal(format!("fence read: {e}")))?;
        let Some((phase, gen)) = current else {
            return Err(Error::rejected("runtime is not known to the owner"));
        };
        if gen as u64 != expected_generation {
            return Err(conflicts("expected generation does not match"));
        }
        if phase == PHASE_REVOKED || phase == PHASE_REVOKING {
            return Ok(());
        }
        tx.execute(
            "UPDATE runtime_obligations SET phase = 'revoking', fenced_reason = ?2, updated_at_ms = ?3 \
             WHERE runtime_id = ?1",
            params![runtime_id, reason, now as i64],
        )
        .map_err(|e| Error::internal(format!("fence update: {e}")))?;
        append_transition(&tx, runtime_id, &phase, PHASE_REVOKING, now)?;
        tx.commit()
            .map_err(|e| Error::internal(format!("fence commit: {e}")))?;
        Ok(())
    }

    /// Acknowledge a fence only after the container is gone: `revoking ->
    /// revoked`. Unknown retirement stays `revoking` (never executable, never
    /// reusable).
    pub(crate) fn mark_revoked(&self, runtime_id: &str) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("revoke transaction: {e}")))?;
        let changed = tx
            .execute(
                "UPDATE runtime_obligations SET phase = 'revoked', updated_at_ms = ?2 \
                 WHERE runtime_id = ?1 AND phase = 'revoking'",
                params![runtime_id, now as i64],
            )
            .map_err(|e| Error::internal(format!("revoke update: {e}")))?;
        if changed != 1 {
            return Err(conflicts("runtime is not revoking"));
        }
        append_transition(&tx, runtime_id, PHASE_REVOKING, PHASE_REVOKED, now)?;
        tx.commit()
            .map_err(|e| Error::internal(format!("revoke commit: {e}")))?;
        Ok(())
    }

    /// Advance the external watermark monotonically. Equal sequence/different
    /// digest refuses; a stale sequence cannot lower it.
    pub(crate) fn checkpoint(
        &self,
        cp: &CheckpointV1,
        prefix: &[LedgerRecordV1],
    ) -> Result<CheckpointAck> {
        cp.validate().map_err(ledger_error)?;
        if cp.ledger_head.sequence.get() == 0 {
            return Err(Error::rejected(
                "checkpoint names the genesis sequence — no mutation to acknowledge",
            ));
        }
        if cp.workspace_id.as_str() != self.workspace_id {
            return Err(Error::rejected("checkpoint workspace does not match"));
        }
        // r3: the native plane selector is `local` (the data custody domain is
        // a separate field and is never conflated with this one).
        if cp.custody_domain.as_str() != "local" {
            return Err(Error::rejected("checkpoint custody domain does not match"));
        }
        if cp.custody_epoch.is_some() {
            return Err(Error::rejected(
                "native owner checkpoint may not carry a hosted custody epoch",
            ));
        }
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("checkpoint transaction: {e}")))?;
        let (epoch, db_epoch): (i64, i64) = tx
            .query_row(
                "SELECT owner_epoch, database_epoch FROM owner_meta WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| Error::internal(format!("checkpoint meta read: {e}")))?;
        let owner = cp
            .owner_epoch
            .ok_or_else(|| Error::rejected("checkpoint has no owner epoch"))?;
        if owner.get() != epoch as u64 || cp.database_epoch.get() != db_epoch as u64 {
            return Err(conflicts(
                "checkpoint database/owner epoch does not match the live authority",
            ));
        }
        // The shared watermark is the only comparison implementation: equal
        // sequence with a different digest and stale sequences refuse; a jump
        // requires the immutable prefix.
        let mut watermark = load_watermark(&tx)?;
        let before_minimum = watermark.minimum_sequence;
        watermark
            .accept_checkpoint(cp, prefix)
            .map_err(ledger_error)?;
        // Equal-head accept_checkpoint does not examine the prefix. Only an
        // empty suffix may accompany it; never persist unexamined rows.
        if cp.ledger_head.sequence == before_minimum && !prefix.is_empty() {
            return Err(Error::rejected(
                "equal-head checkpoint must not carry ledger records",
            ));
        }
        persist_watermark(&tx, &watermark)?;
        // Persist every record head the shared chain just verified, so a later
        // older-before-newer writer can obtain an exact read-only receipt.
        for record in prefix {
            verify_or_insert_checkpoint_row(
                &tx,
                &record.sequence,
                &record.digest,
                &record.database_epoch,
                &cp.runtime_binding_digest,
                now,
            )?;
        }
        verify_or_insert_checkpoint_row(
            &tx,
            &cp.ledger_head.sequence,
            &cp.ledger_head.digest,
            &cp.database_epoch,
            &cp.runtime_binding_digest,
            now,
        )?;
        tx.commit()
            .map_err(|e| Error::internal(format!("checkpoint commit: {e}")))?;
        Ok(CheckpointAck {
            v: cadence_core::connection_dispatch::ledger::LEDGER_CONTRACT_VERSION,
            workspace_id: cp.workspace_id.clone(),
            custody_domain: cp.custody_domain,
            database_epoch: cp.database_epoch,
            sequence: cp.ledger_head.sequence,
            digest: cp.ledger_head.digest.clone(),
        })
    }

    /// Read-only receipt for an exact ledger head the owner has already
    /// verified through the shared chain. Never writes the watermark, never
    /// inserts a checkpoint, and never lowers/raises either. Used only for the
    /// older-before-newer concurrent case where the authenticated watermark
    /// already advanced past this head.
    pub(crate) fn checkpoint_receipt(&self, cp: &CheckpointV1) -> Result<CheckpointAck> {
        // A receipt refusal is never a mutation-kind "conflict": malformed,
        // absent or conflicting receipt evidence is a definite rejection.
        cp.validate()
            .map_err(|_| Error::rejected("receipt checkpoint is malformed"))?;
        if cp.workspace_id.as_str() != self.workspace_id {
            return Err(Error::rejected("receipt workspace does not match"));
        }
        if cp.custody_domain.as_str() != "local" {
            return Err(Error::rejected("receipt custody domain does not match"));
        }
        if cp.custody_epoch.is_some() {
            return Err(Error::rejected(
                "native owner receipt may not carry a hosted custody epoch",
            ));
        }
        if cp.ledger_head.sequence.get() == 0 {
            return Err(Error::rejected(
                "receipt names the genesis sequence — no mutation to acknowledge",
            ));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("receipt transaction: {e}")))?;
        let (live_owner_epoch, live_database_epoch, restore_state): (i64, i64, String) = tx
            .query_row(
                "SELECT owner_epoch, database_epoch, restore_state FROM owner_meta WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| Error::internal(format!("receipt meta read: {e}")))?;
        let owner_epoch = cp
            .owner_epoch
            .ok_or_else(|| Error::rejected("receipt has no owner epoch"))?;
        if owner_epoch.get() != live_owner_epoch as u64
            || cp.database_epoch.get() != live_database_epoch as u64
        {
            return Err(Error::rejected(
                "receipt database/owner epoch does not match the live authority",
            ));
        }
        if restore_state != "stable" {
            return Err(Error::rejected(
                "receipt requires a stable owner restore state",
            ));
        }
        let watermark = load_watermark(&tx)?;
        if cp.ledger_head.sequence > watermark.minimum_sequence {
            return Err(Error::rejected(
                "receipt head is newer than the owner watermark",
            ));
        }
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT digest, database_epoch FROM owner_checkpoints WHERE sequence = ?1",
                params![cp.ledger_head.sequence.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| Error::internal(format!("receipt read: {e}")))?;
        let Some((digest, database_epoch)) = row else {
            return Err(Error::rejected(
                "no owner receipt exists for this exact ledger head",
            ));
        };
        if digest != cp.ledger_head.digest.as_str()
            || database_epoch != cp.database_epoch.get() as i64
        {
            return Err(Error::rejected(
                "owner receipt does not match the exact ledger head",
            ));
        }
        let ack = CheckpointAck {
            v: cadence_core::connection_dispatch::ledger::LEDGER_CONTRACT_VERSION,
            workspace_id: cp.workspace_id.clone(),
            custody_domain: cp.custody_domain,
            database_epoch: cp.database_epoch,
            sequence: cp.ledger_head.sequence,
            digest: cp.ledger_head.digest.clone(),
        };
        tx.commit()
            .map_err(|e| Error::internal(format!("receipt commit: {e}")))?;
        Ok(ack)
    }

    /// `restore_begin` — only the measured installer. Records the checkpoint,
    /// fences the current epoch, preserves all history and requires a new
    /// epoch before readiness.
    pub(crate) fn restore_begin(
        &self,
        cp: &CheckpointV1,
        prefix: &[LedgerRecordV1],
        reason: &str,
    ) -> Result<u64> {
        self.checkpoint(cp, prefix)?;
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("restore_begin transaction: {e}")))?;
        let epoch: i64 = tx
            .query_row("SELECT owner_epoch FROM owner_meta WHERE id = 1", [], |r| {
                r.get(0)
            })
            .map_err(|e| Error::internal(format!("restore_begin epoch: {e}")))?;
        let next = epoch as u64 + 1;
        if next > super::MAX_SAFE_INT {
            return Err(Error::rejected("owner epoch exhausted"));
        }
        fence_every_runtime(&tx, reason, now)?;
        tx.execute(
            "UPDATE owner_meta SET restore_state = 'restoring', owner_epoch = ?1 WHERE id = 1",
            params![next as i64],
        )
        .map_err(|e| Error::internal(format!("restore_begin meta: {e}")))?;
        tx.execute(
            "INSERT INTO restore_log (action, owner_epoch, database_epoch, at_ms) VALUES ('begin', ?1, ?2, ?3)",
            params![next as i64, cp.database_epoch.get() as i64, now as i64],
        )
        .map_err(|e| Error::internal(format!("restore_begin log: {e}")))?;
        tx.commit()
            .map_err(|e| Error::internal(format!("restore_begin commit: {e}")))?;
        Ok(next)
    }

    /// `restore_complete` — requires the preserved history to satisfy the
    /// external watermark and a strictly newer epoch; leaves all grants and
    /// runtimes revoked, never restores admission.
    pub(crate) fn restore_complete(&self, expected_epoch: u64) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("restore_complete transaction: {e}")))?;
        let (epoch, state): (i64, String) = tx
            .query_row(
                "SELECT owner_epoch, restore_state FROM owner_meta WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| Error::internal(format!("restore_complete read: {e}")))?;
        if state != "restoring" {
            return Err(Error::rejected("owner is not restoring"));
        }
        if epoch as u64 != expected_epoch {
            return Err(conflicts("restore epoch does not match"));
        }
        // The preserved history must still contain the exact watermark head;
        // a missing/forked prefix refuses recovery instead of resetting the
        // external floor.
        let watermark = load_watermark(&tx)?;
        if watermark.minimum_sequence.get() > 0 {
            let recorded: Option<String> = tx
                .query_row(
                    "SELECT digest FROM owner_checkpoints WHERE sequence = ?1",
                    params![watermark.minimum_sequence.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| Error::internal(format!("restore checkpoint read: {e}")))?;
            if recorded.as_deref() != Some(watermark.minimum_digest.as_str()) {
                return Err(Error::rejected(
                    "preserved history does not satisfy the external watermark",
                ));
            }
        }
        tx.execute(
            "UPDATE owner_meta SET restore_state = 'stable' WHERE id = 1",
            [],
        )
        .map_err(|e| Error::internal(format!("restore_complete meta: {e}")))?;
        tx.execute(
            "INSERT INTO restore_log (action, owner_epoch, database_epoch, at_ms) \
             SELECT 'complete', owner_epoch, database_epoch, ?1 FROM owner_meta WHERE id = 1",
            params![now as i64],
        )
        .map_err(|e| Error::internal(format!("restore_complete log: {e}")))?;
        tx.commit()
            .map_err(|e| Error::internal(format!("restore_complete commit: {e}")))?;
        Ok(())
    }

    pub(crate) fn record_peer(
        &self,
        role: &str,
        generation: &str,
        peer: &super::peer::Peer,
    ) -> Result<()> {
        let now = now_ms();
        let guard = self.lock()?;
        db(guard.execute(
            "INSERT OR REPLACE INTO peer_enrollments (role, generation, pid, starttime, exe_digest, enrolled_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                role,
                generation,
                peer.pid as i64,
                peer.starttime as i64,
                super::peer::hex(&peer.exe_digest),
                now as i64
            ],
        ))?;
        Ok(())
    }

    /// The pinned generation for a live peer identity, if enrolled.
    pub(crate) fn peer_generation(
        &self,
        role: &str,
        pid: u32,
        starttime: u64,
    ) -> Result<Option<String>> {
        let guard = self.lock()?;
        guard
            .query_row(
                "SELECT generation FROM peer_enrollments WHERE role = ?1 AND pid = ?2 AND starttime = ?3",
                params![role, pid as i64, starttime as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| Error::internal(format!("peer enrollment read: {e}")))
    }

    /// Every obligation that may still own a container, for reconciliation.
    pub(crate) fn live_obligations(&self) -> Result<Vec<Obligation>> {
        let guard = self.lock()?;
        let mut stmt = db(guard.prepare(&format!(
            "SELECT {OBLIGATION_COLUMNS} FROM runtime_obligations \
             WHERE phase NOT IN ('revoked') ORDER BY created_at_ms ASC"
        )))?;
        let rows = db(stmt.query_map([], row_to_obligation))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(db(row)?);
        }
        Ok(out)
    }

    fn cas(
        &self,
        runtime_id: &str,
        from: &str,
        to: &str,
        challenge_digest: Option<&str>,
    ) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("transition transaction: {e}")))?;
        let changed = match challenge_digest {
            Some(digest) => tx
                .execute(
                    "UPDATE runtime_obligations SET phase = ?3, updated_at_ms = ?4 \
                     WHERE runtime_id = ?1 AND phase = ?2 AND challenge_digest = ?5",
                    params![runtime_id, from, to, now as i64, digest],
                )
                .map_err(|e| Error::internal(format!("transition update: {e}")))?,
            None => tx
                .execute(
                    "UPDATE runtime_obligations SET phase = ?3, updated_at_ms = ?4 \
                     WHERE runtime_id = ?1 AND phase = ?2",
                    params![runtime_id, from, to, now as i64],
                )
                .map_err(|e| Error::internal(format!("transition update: {e}")))?,
        };
        if changed != 1 {
            return Err(conflicts(format!(
                "runtime is not {from} for the requested transition"
            )));
        }
        append_transition(&tx, runtime_id, from, to, now)?;
        tx.commit()
            .map_err(|e| Error::internal(format!("transition commit: {e}")))?;
        Ok(())
    }

    fn cas_generation(
        &self,
        runtime_id: &str,
        from: &str,
        to: &str,
        generation: u64,
    ) -> Result<()> {
        let now = now_ms();
        let mut guard = self.lock()?;
        let tx = guard
            .transaction()
            .map_err(|e| Error::internal(format!("transition transaction: {e}")))?;
        let changed = tx
            .execute(
                "UPDATE runtime_obligations SET phase = ?3, updated_at_ms = ?4 \
                 WHERE runtime_id = ?1 AND phase = ?2 AND generation = ?5",
                params![runtime_id, from, to, now as i64, generation as i64],
            )
            .map_err(|e| Error::internal(format!("transition update: {e}")))?;
        if changed != 1 {
            return Err(conflicts(format!(
                "runtime is not {from} at the requested generation"
            )));
        }
        append_transition(&tx, runtime_id, from, to, now)?;
        tx.commit()
            .map_err(|e| Error::internal(format!("transition commit: {e}")))?;
        Ok(())
    }
}

fn append_transition(
    tx: &rusqlite::Transaction<'_>,
    runtime_id: &str,
    from: &str,
    to: &str,
    now: u64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO obligation_transitions (runtime_id, from_phase, to_phase, at_ms) \
         VALUES (?1, ?2, ?3, ?4)",
        params![runtime_id, from, to, now as i64],
    )
    .map_err(|e| Error::internal(format!("transition append: {e}")))?;
    Ok(())
}

/// Fence every runtime that is not already revoking/revoked, appending one
/// transition per row (the phase it was actually in). Restart/restore retain
/// quarantine and capacity until broker ACK and retirement are confirmed;
/// epoch advancement never frees a UID/name or manufactures retirement.
fn fence_every_runtime(tx: &rusqlite::Transaction<'_>, reason: &str, now: u64) -> Result<()> {
    let mut fences: Vec<(String, String)> = Vec::new();
    {
        let mut stmt = tx
            .prepare(
                "SELECT runtime_id, phase FROM runtime_obligations \
                 WHERE phase NOT IN ('revoking','revoked')",
            )
            .map_err(|e| Error::internal(format!("fence scan: {e}")))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| Error::internal(format!("fence scan: {e}")))?;
        for row in rows {
            fences.push(row.map_err(|e| Error::internal(format!("fence row: {e}")))?);
        }
    }
    for (runtime_id, phase) in fences {
        tx.execute(
            "UPDATE runtime_obligations SET phase = 'revoking', fenced_reason = ?2, \
             updated_at_ms = ?3 WHERE runtime_id = ?1",
            params![runtime_id, reason, now as i64],
        )
        .map_err(|e| Error::internal(format!("fence update: {e}")))?;
        append_transition(tx, &runtime_id, &phase, PHASE_REVOKING, now)?;
    }
    Ok(())
}

fn ledger_error(e: LedgerError) -> Error {
    match e {
        LedgerError::Stale => Error::conflict_coded(
            "owner_watermark_stale",
            "checkpoint is older than the recorded external watermark",
        ),
        LedgerError::Mismatch => Error::conflict_coded(
            "owner_watermark_mismatch",
            "checkpoint does not match the authoritative ledger prefix",
        ),
        LedgerError::EpochRegression => {
            Error::rejected("checkpoint database epoch moved backwards")
        }
        LedgerError::SequenceExhausted => Error::rejected("ledger sequence exhausted"),
        LedgerError::Malformed => Error::rejected("checkpoint is malformed"),
    }
}

fn load_watermark(conn: &Connection) -> Result<LedgerWatermarkV1> {
    let (seq, digest, db): (String, String, i64) = conn
        .query_row(
            "SELECT minimum_sequence, minimum_digest, database_epoch FROM owner_watermark WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| Error::internal(format!("watermark read: {e}")))?;
    Ok(LedgerWatermarkV1 {
        minimum_sequence: Counter::try_from(seq)
            .map_err(|_| Error::rejected("watermark sequence is malformed"))?,
        minimum_digest: Digest::try_from(digest)
            .map_err(|_| Error::rejected("watermark digest is malformed"))?,
        database_epoch: Revision::new(db as u64)
            .map_err(|_| Error::rejected("watermark database epoch is malformed"))?,
    })
}

fn verify_or_insert_checkpoint_row(
    conn: &Connection,
    sequence: &Counter,
    digest: &Digest,
    database_epoch: &Revision,
    runtime_binding_digest: &Digest,
    at_ms: u64,
) -> Result<()> {
    let sequence_text = sequence.to_string();
    let existing: Option<(String, i64)> = conn
        .query_row(
            "SELECT digest, database_epoch FROM owner_checkpoints WHERE sequence = ?1",
            params![&sequence_text],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| Error::internal(format!("checkpoint row read: {e}")))?;
    if let Some((existing_digest, existing_epoch)) = existing {
        if existing_digest != digest.as_str() || existing_epoch != database_epoch.get() as i64 {
            return Err(conflicts(
                "owner checkpoint history conflicts for this sequence",
            ));
        }
        return Ok(());
    }
    conn.execute(
        "INSERT INTO owner_checkpoints (sequence, digest, database_epoch, \
         runtime_binding_digest, at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            sequence_text,
            digest.as_str(),
            database_epoch.get() as i64,
            runtime_binding_digest.as_str(),
            at_ms as i64
        ],
    )
    .map_err(|e| Error::internal(format!("checkpoint row insert: {e}")))?;
    Ok(())
}

fn persist_watermark(conn: &Connection, watermark: &LedgerWatermarkV1) -> Result<()> {
    conn.execute(
        "INSERT INTO owner_watermark (id, minimum_sequence, minimum_digest, database_epoch) \
         VALUES (1, ?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET \
         minimum_sequence = excluded.minimum_sequence, \
         minimum_digest = excluded.minimum_digest, \
         database_epoch = excluded.database_epoch",
        params![
            watermark.minimum_sequence.to_string(),
            watermark.minimum_digest.as_str(),
            watermark.database_epoch.get() as i64
        ],
    )
    .map_err(|e| Error::internal(format!("watermark write: {e}")))?;
    Ok(())
}

const OBLIGATION_COLUMNS: &str = "runtime_id, operation_id, generation, challenge_json, challenge_digest, \
 profile_digest, owner_epoch, phase, agent_uid, session_binding, server_generation, os_uid, os_gid, \
 container_name, container_id, ops_socket, attest_socket, observation_counter, observed_registry_digest, \
 observed_registry_json, loaded_receipt_id, node_pid, node_starttime, qualified_at_ms, fenced_reason, \
 created_at_ms, updated_at_ms, launch_request_json, launch_request_digest, recipient_pid, \
 recipient_starttime, recipient_generation, recipient_nonce, installer_pid, installer_starttime, \
 installer_generation";

fn row_to_obligation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Obligation> {
    Ok(Obligation {
        runtime_id: row.get(0)?,
        operation_id: row.get(1)?,
        generation: row.get::<_, i64>(2)? as u64,
        challenge_json: row.get(3)?,
        challenge_digest: row.get(4)?,
        profile_digest: row.get(5)?,
        owner_epoch: row.get::<_, i64>(6)? as u64,
        phase: row.get(7)?,
        agent_uid: row.get(8)?,
        session_binding: row.get(9)?,
        server_generation: row.get::<_, i64>(10)? as u64,
        os_uid: row.get::<_, i64>(11)? as u32,
        os_gid: row.get::<_, i64>(12)? as u32,
        container_name: row.get(13)?,
        container_id: row.get(14)?,
        ops_socket: row.get(15)?,
        attest_socket: row.get(16)?,
        observation_counter: row.get::<_, i64>(17)? as u64,
        observed_registry_digest: row.get(18)?,
        observed_registry_json: row.get(19)?,
        loaded_receipt_id: row.get(20)?,
        node_pid: row.get::<_, Option<i64>>(21)?.map(|v| v as u32),
        node_starttime: row.get::<_, Option<i64>>(22)?.map(|v| v as u64),
        qualified_at_ms: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
        fenced_reason: row.get(24)?,
        created_at_ms: row.get::<_, i64>(25)? as u64,
        updated_at_ms: row.get::<_, i64>(26)? as u64,
        launch_request_json: row.get(27)?,
        launch_request_digest: row.get(28)?,
        recipient_pid: row.get::<_, Option<i64>>(29)?.map(|v| v as u32),
        recipient_starttime: row.get::<_, Option<i64>>(30)?.map(|v| v as u64),
        recipient_generation: row.get(31)?,
        recipient_nonce: row.get(32)?,
        installer_pid: row.get::<_, Option<i64>>(33)?.map(|v| v as u32),
        installer_starttime: row.get::<_, Option<i64>>(34)?.map(|v| v as u64),
        installer_generation: row.get(35)?,
    })
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|_| Error::internal("owner state directory cannot be created"))?;
    let mut perms = std::fs::metadata(dir)
        .map_err(|_| Error::internal("owner state directory unreadable"))?
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o700);
    std::fs::set_permissions(dir, perms)
        .map_err(|_| Error::internal("owner state directory cannot be sealed"))?;
    Ok(())
}

fn set_file_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|_| Error::internal("owner database metadata unreadable"))?
        .permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms)
        .map_err(|_| Error::internal("owner database cannot be sealed"))?;
    Ok(())
}

/// Verify the runtime-owner's private database file is still root-owned and
/// not group/other accessible (a later `chmod`/copy must not widen it).
pub(crate) fn verify_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| Error::rejected("owner authority database is missing"))?;
    if !meta.is_file() || meta.is_symlink() {
        return Err(Error::rejected(
            "owner authority database must be a regular file",
        ));
    }
    if meta.uid() != 0 {
        return Err(Error::rejected(
            "owner authority database must be owned by root",
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(Error::rejected(
            "owner authority database is accessible to group/other",
        ));
    }
    Ok(())
}

const SCHEMA_V1: &str = "\
CREATE TABLE owner_meta (\
 id INTEGER PRIMARY KEY CHECK (id = 1),\
 workspace_id TEXT NOT NULL,\
 owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),\
 boot_id TEXT NOT NULL,\
 started_at_ms INTEGER NOT NULL,\
 database_epoch INTEGER NOT NULL CHECK (database_epoch > 0),\
 restore_state TEXT NOT NULL CHECK (restore_state IN ('stable','restoring','fenced')),\
 created_at_ms INTEGER NOT NULL\
);\
CREATE TABLE uid_allocations (\
 os_uid INTEGER PRIMARY KEY,\
 runtime_id TEXT NOT NULL UNIQUE\
);\
CREATE TABLE runtime_obligations (\
 runtime_id TEXT PRIMARY KEY,\
 operation_id TEXT NOT NULL UNIQUE,\
 generation INTEGER NOT NULL UNIQUE CHECK (generation > 0),\
 challenge_json TEXT,\
 challenge_digest TEXT UNIQUE,\
 profile_digest TEXT NOT NULL,\
 owner_epoch INTEGER NOT NULL,\
 phase TEXT NOT NULL CHECK (phase IN ('allocating','prepared','consumed','starting','observing','ready','unknown','revoking','revoked')),\
 agent_uid TEXT NOT NULL,\
 session_binding TEXT NOT NULL,\
 server_generation INTEGER NOT NULL,\
 os_uid INTEGER NOT NULL UNIQUE,\
 os_gid INTEGER NOT NULL,\
 container_name TEXT NOT NULL UNIQUE,\
 container_id TEXT,\
 ops_socket TEXT NOT NULL UNIQUE,\
 attest_socket TEXT NOT NULL UNIQUE,\
 observation_key BLOB,\
 observation_counter INTEGER NOT NULL DEFAULT 0,\
 observed_registry_digest TEXT,\
 observed_registry_json TEXT,\
 loaded_receipt_id TEXT,\
 node_pid INTEGER,\
 node_starttime INTEGER,\
 started_at_ms INTEGER,\
 qualified_at_ms INTEGER,\
 fenced_reason TEXT,\
 created_at_ms INTEGER NOT NULL,\
 updated_at_ms INTEGER NOT NULL,\
 launch_request_json TEXT,\
 launch_request_digest TEXT UNIQUE,\
 recipient_pid INTEGER,\
 recipient_starttime INTEGER,\
 recipient_generation TEXT,\
 recipient_nonce TEXT,\
 installer_pid INTEGER,\
 installer_starttime INTEGER,\
 installer_generation TEXT\
);\
CREATE TABLE obligation_transitions (\
 id INTEGER PRIMARY KEY AUTOINCREMENT,\
 runtime_id TEXT NOT NULL,\
 from_phase TEXT,\
 to_phase TEXT NOT NULL,\
 at_ms INTEGER NOT NULL\
);\
CREATE TRIGGER obligation_transitions_no_update BEFORE UPDATE ON obligation_transitions \
 BEGIN SELECT RAISE(ABORT, 'append-only'); END;\
CREATE TRIGGER obligation_transitions_no_delete BEFORE DELETE ON obligation_transitions \
 BEGIN SELECT RAISE(ABORT, 'append-only'); END;\
CREATE TABLE owner_checkpoints (\
 sequence TEXT PRIMARY KEY,\
 digest TEXT NOT NULL,\
 database_epoch INTEGER NOT NULL,\
 runtime_binding_digest TEXT NOT NULL,\
 at_ms INTEGER NOT NULL\
);\
CREATE TABLE owner_watermark (\
 id INTEGER PRIMARY KEY CHECK (id = 1),\
 minimum_sequence TEXT NOT NULL,\
 minimum_digest TEXT NOT NULL,\
 database_epoch INTEGER NOT NULL CHECK (database_epoch > 0)\
);\
CREATE TABLE restore_log (\
 id INTEGER PRIMARY KEY AUTOINCREMENT,\
 action TEXT NOT NULL CHECK (action IN ('begin','complete')),\
 owner_epoch INTEGER NOT NULL,\
 database_epoch INTEGER NOT NULL,\
 at_ms INTEGER NOT NULL\
);\
CREATE TABLE peer_enrollments (\
 role TEXT NOT NULL CHECK (role IN ('supervisor','installer')),\
 generation TEXT NOT NULL,\
 pid INTEGER NOT NULL,\
 starttime INTEGER NOT NULL,\
 exe_digest TEXT NOT NULL,\
 enrolled_at_ms INTEGER NOT NULL,\
 PRIMARY KEY (role, pid, starttime)\
);";

/// Domain digest of the exact canonical challenge bytes, used as the durable
/// obligation selector. This never replaces the canonical grant encoding; it
/// only names the stored obligation row.
pub(crate) fn challenge_digest(bytes: &[u8]) -> String {
    let mut material = Vec::with_capacity(bytes.len() + 32);
    material.extend_from_slice(b"cadence.runtime-owner.challenge.v1\0");
    material.extend_from_slice(bytes);
    profile::sha256_digest(&material)
}

#[cfg(test)]
#[path = "store_quarantine_acceptance_20261027.rs"]
mod store_quarantine_acceptance_20261027;

#[cfg(test)]
#[path = "checkpoint_receipt_acceptance_20261027.rs"]
mod checkpoint_receipt_acceptance_20261027;

#[cfg(test)]
#[path = "store_grant_recheck_acceptance_20261027.rs"]
mod store_grant_recheck_acceptance_20261027;
