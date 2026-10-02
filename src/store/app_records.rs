//! Host-managed per-installation app record files (CAD-753).
//!
//! Each installation owns one physical SQLite file,
//! `<state_dir>/app-records/<install_id>.sqlite3`. Contexts share
//! their installation's file; rows are scoped by `(context_id, id)`
//! inside it. Core Cadence keeps the catalog, caller identity,
//! grants, run and effect state — record data and history never land
//! in core SQLite or the tracker Git tree.
//!
//! App code, frames, chat and workers never see a path or SQL — only
//! the typed operator actions in `daemon::app_records_rpc` open these
//! files, after proving the operator connection, the installation and
//! (for writes) the live context. The daemon coordinates with core
//! without a cross-file transaction: the context proof is checked on
//! core first, then the record write commits entirely inside the
//! installation file. Backup/export of the record directory is
//! follow-up CAD-753-F2; initialization here is recoverable (missing
//! files are created, corrupt or foreign files are refused, never
//! deleted).

use super::*;
use crate::store::app_runs::material_digest;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use super::StoreConn;

pub const RECORDS_DIR: &str = "app-records";
/// Schema version of each per-installation record file. Backup manifests
/// bind this per file; restore refuses a newer version than this binary.
pub const FILE_SCHEMA: i64 = 1;

pub const RECORD_BODY_BYTES: usize = 16 * 1024;
pub const RECORD_LIMIT: i64 = 100;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS record_schema(version INTEGER NOT NULL);
INSERT INTO record_schema(version)
  SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM record_schema);
CREATE TABLE IF NOT EXISTS record_identity(
 install_id TEXT PRIMARY KEY, created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS app_records(
 context_id TEXT NOT NULL, id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('customer')),
 revision INTEGER NOT NULL CHECK(revision>0),
 body TEXT NOT NULL, body_digest TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL,
 PRIMARY KEY(context_id, id));
CREATE INDEX IF NOT EXISTS app_records_context ON app_records(context_id);
CREATE TABLE IF NOT EXISTS app_record_revisions(
 context_id TEXT NOT NULL, record_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 body TEXT NOT NULL, body_digest TEXT NOT NULL,
 actor TEXT NOT NULL, at REAL NOT NULL,
 PRIMARY KEY(context_id, record_id, revision));
CREATE TABLE IF NOT EXISTS app_record_csv_imports(
 request_id TEXT PRIMARY KEY, context_id TEXT NOT NULL,
 preview_token TEXT NOT NULL, result TEXT NOT NULL,
 state TEXT NOT NULL, at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS app_segments(
 context_id TEXT NOT NULL, id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 name TEXT NOT NULL, definition TEXT NOT NULL, digest TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL,
 PRIMARY KEY(context_id, id));
CREATE TABLE IF NOT EXISTS app_segment_revisions(
 context_id TEXT NOT NULL, segment_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 definition TEXT NOT NULL, digest TEXT NOT NULL,
 actor TEXT NOT NULL, at REAL NOT NULL,
 PRIMARY KEY(context_id, segment_id, revision));
CREATE TABLE IF NOT EXISTS app_exclusions(
 context_id TEXT NOT NULL, id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 name TEXT NOT NULL, member_ids TEXT NOT NULL, digest TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL,
 PRIMARY KEY(context_id, id));
CREATE TABLE IF NOT EXISTS app_exclusion_revisions(
 context_id TEXT NOT NULL, list_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 member_ids TEXT NOT NULL, digest TEXT NOT NULL,
 actor TEXT NOT NULL, at REAL NOT NULL,
 PRIMARY KEY(context_id, list_id, revision));
CREATE TABLE IF NOT EXISTS app_suppressions(
 context_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('email','customer')),
 key TEXT NOT NULL, reason TEXT NOT NULL, at REAL NOT NULL,
 PRIMARY KEY(context_id, kind, key));
CREATE TABLE IF NOT EXISTS app_audience_freezes(
 context_id TEXT NOT NULL, freeze_id TEXT NOT NULL,
 base TEXT NOT NULL, exclusion_list_id TEXT,
 member_ids TEXT NOT NULL, digest TEXT NOT NULL,
 max_recipients INTEGER NOT NULL, pins TEXT NOT NULL,
 created REAL NOT NULL, PRIMARY KEY(context_id, freeze_id));
CREATE TABLE IF NOT EXISTS app_content_docs(
 context_id TEXT NOT NULL, campaign_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 subject TEXT NOT NULL, preheader TEXT NOT NULL,
 blocks TEXT NOT NULL, content_digest TEXT NOT NULL,
 approval_revision INTEGER, approval_digest TEXT,
 actor TEXT NOT NULL, created REAL NOT NULL, updated REAL NOT NULL,
 PRIMARY KEY(context_id, campaign_id));
CREATE TABLE IF NOT EXISTS app_content_revisions(
 context_id TEXT NOT NULL, campaign_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 subject TEXT NOT NULL, preheader TEXT NOT NULL,
 blocks TEXT NOT NULL, content_digest TEXT NOT NULL,
 actor TEXT NOT NULL, origin TEXT NOT NULL CHECK(origin IN ('operator','proposal')),
 proposal_id TEXT, at REAL NOT NULL,
 PRIMARY KEY(context_id, campaign_id, revision));
CREATE TABLE IF NOT EXISTS app_content_proposals(
 context_id TEXT NOT NULL, proposal_id TEXT NOT NULL,
 campaign_id TEXT NOT NULL, source_revision INTEGER NOT NULL CHECK(source_revision>=0),
 subject TEXT NOT NULL, preheader TEXT NOT NULL,
 blocks TEXT NOT NULL, content_digest TEXT NOT NULL,
 actor TEXT NOT NULL, origin TEXT NOT NULL CHECK(origin IN ('operator-direct','assistant-receipt')),
 receipt_message TEXT, receipt_agent TEXT, receipt_request TEXT,
 state TEXT NOT NULL CHECK(state IN ('pending','applied','discarded')),
 created REAL NOT NULL, decided REAL,
 PRIMARY KEY(context_id, proposal_id));
CREATE INDEX IF NOT EXISTS app_content_proposals_campaign ON app_content_proposals(context_id,campaign_id);
CREATE UNIQUE INDEX IF NOT EXISTS app_content_proposal_claim ON app_content_proposals(context_id, receipt_message);
CREATE TABLE IF NOT EXISTS app_content_proposal_requests(
 context_id TEXT NOT NULL, request_id TEXT NOT NULL,
 campaign_id TEXT NOT NULL, source_revision INTEGER NOT NULL CHECK(source_revision>=0),
 message_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('open','used')),
 used_by TEXT, created REAL NOT NULL, decided REAL,
 PRIMARY KEY(context_id, request_id));
CREATE TABLE IF NOT EXISTS app_sender_bindings(
 context_id TEXT NOT NULL, binding_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 sender_name TEXT NOT NULL, sender_address TEXT NOT NULL,
 unsubscribe_base TEXT NOT NULL, connection_id TEXT,
 binding_digest TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL,
 PRIMARY KEY(context_id, binding_id));
CREATE TABLE IF NOT EXISTS app_sender_binding_revisions(
 context_id TEXT NOT NULL, binding_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 sender_name TEXT NOT NULL, sender_address TEXT NOT NULL,
 unsubscribe_base TEXT NOT NULL, connection_id TEXT,
 binding_digest TEXT NOT NULL, actor TEXT NOT NULL, at REAL NOT NULL,
 PRIMARY KEY(context_id, binding_id, revision));
CREATE TABLE IF NOT EXISTS app_campaign_test_sends(
 context_id TEXT NOT NULL, campaign_id TEXT NOT NULL,
 content_digest TEXT NOT NULL, link_digest TEXT NOT NULL,
 accepted INTEGER NOT NULL, at REAL NOT NULL,
 PRIMARY KEY(context_id,campaign_id,content_digest,link_digest));
CREATE TABLE IF NOT EXISTS app_campaign_sends(
 context_id TEXT NOT NULL, send_id TEXT NOT NULL,
 campaign_id TEXT NOT NULL, request_id TEXT NOT NULL,
 content_revision INTEGER NOT NULL, content_digest TEXT NOT NULL,
 audience_freeze_id TEXT NOT NULL, audience_digest TEXT NOT NULL,
 connection_id TEXT NOT NULL, auth_revision INTEGER NOT NULL,
 link_revision INTEGER NOT NULL, link_digest TEXT NOT NULL,
 max_recipients INTEGER NOT NULL, send_digest TEXT NOT NULL,
 unsubscribe_origin TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('prepared','sending','completed','closed')),
 close_reason TEXT, created REAL NOT NULL, updated REAL NOT NULL,
 approved_at REAL,
 PRIMARY KEY(context_id,send_id),
 UNIQUE(context_id,request_id));
CREATE TABLE IF NOT EXISTS app_campaign_deliveries(
 context_id TEXT NOT NULL, send_id TEXT NOT NULL,
 customer_id TEXT NOT NULL, email TEXT NOT NULL,
 idempotency_key TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','submitting','accepted','failed','uncertain','suppressed','closed')),
 attempts INTEGER NOT NULL DEFAULT 0,
 smtp_code INTEGER, smtp_message TEXT, reason TEXT, resolved_by TEXT,
 updated REAL NOT NULL,
 PRIMARY KEY(context_id,send_id,customer_id),
 UNIQUE(idempotency_key));
CREATE TABLE IF NOT EXISTS app_unsubscribe_tokens(
 token_hash TEXT PRIMARY KEY,
 context_id TEXT NOT NULL, customer_id TEXT NOT NULL, send_id TEXT NOT NULL,
 created REAL NOT NULL);
";

/// The record file for an installation. The identifier grammar
/// (`[a-z0-9-]`, no separators) makes traversal impossible by
/// construction; anything else is refused before the path is built.
pub fn record_db_path(state_dir: &Path, install_id: &str) -> Result<PathBuf> {
    crate::proto::identifier(install_id, "installation ID")?;
    Ok(state_dir
        .join(RECORDS_DIR)
        .join(format!("{install_id}.sqlite3")))
}

/// Lock contention at open time, classified by SQLite error code
/// — never by human-readable text. `DatabaseBusy`/`DatabaseLocked`
/// at any first-open point becomes the bounded-retry signal; every
/// other failure keeps its existing immediate refusal.
fn is_contention(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked)
    )
}

/// The bounded-retry signals for `open`, matched exactly. A lost
/// init race, a half-committed file, and coded lock contention may
/// retry; every other refusal (corruption, identity, schema,
/// permissions) fails closed on the first attempt.
fn is_transient_open_error(error: &Error) -> bool {
    matches!(
        error.to_string().as_str(),
        "record file initialization diverged"
            | "record file initialization in progress"
            | "record file is busy"
    )
}

/// A lost fresh-init race, classified by typed SQLite extended
/// code: only a PRIMARYKEY violation on the identity insert means
/// a sibling won between our version check and the insert — the
/// identity schema declares install_id TEXT PRIMARY KEY, so a
/// duplicate insert reports SQLITE_CONSTRAINT_PRIMARYKEY (verified
/// against a live duplicate insert, not assumed). Any other
/// failure — including one whose message happens to mention
/// uniqueness — fails closed through the caller's refusal.
fn is_lost_init(error: &rusqlite::Error) -> bool {
    error
        .sqlite_error()
        .is_some_and(|detail| detail.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY)
}

/// Map a rusqlite failure at open time: coded contention becomes
/// the retry signal, everything else keeps its existing refusal.
fn busy_or(error: &rusqlite::Error, otherwise: Error) -> Error {
    if is_contention(error) {
        Error::internal("record file is busy")
    } else {
        otherwise
    }
}

/// Fail-closed diagnosis for a foreign, corrupt, downgraded, or
/// never-initialized record file. Shared by every refusal site so
/// the operator-facing text stays identical.
const CORRUPT_RECORD_FILE: &str = "record file is corrupt or foreign; restore the installation backup or remove the file after inspection";

/// One installation's record file. Opened per operator action and
/// closed after it — persistence is the file itself, shared through
/// SQLite locking, never daemon memory.
pub struct RecordStore {
    install_id: String,
    conn: Mutex<Connection>,
}

impl RecordStore {
    pub(crate) fn install(&self) -> &str {
        &self.install_id
    }
    /// Open (creating and initializing when missing) the
    /// installation's record file. A present file whose schema or
    /// identity row does not match is refused with an explicit
    /// recovery error — user data is never deleted or rewritten.
    pub fn open(state_dir: &Path, install_id: &str) -> Result<Self> {
        // Concurrent first opens race the fresh initialization and
        // SQLite locking. The three transient signals above retry
        // through one single bounded wait (40 x 50ms of sleep here;
        // each attempt's own SQLite busy handler may additionally
        // wait up to BUSY_TIMEOUT under sustained contention). A
        // file still table-less when the wait expires is refused as
        // corrupt — never reported as still initializing. Genuine
        // corruption, identity mismatch, unsupported schema or an
        // unwritable directory never match and refuse immediately.
        let mut attempt = 0;
        loop {
            match Self::open_once(state_dir, install_id) {
                Err(error) if is_transient_open_error(&error) && attempt < 40 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) if error.to_string() == "record file initialization in progress" => {
                    return Err(Error::rejected(CORRUPT_RECORD_FILE));
                }
                settled => return settled,
            }
        }
    }

    fn open_once(state_dir: &Path, install_id: &str) -> Result<Self> {
        let path = record_db_path(state_dir, install_id)?;
        if let Some(parent) = path.parent() {
            if !parent.is_dir() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .map_err(|e| {
                        Error::internal(format!("record store directory unavailable: {e}"))
                    })?;
            }
        }
        // A present file whose bytes SQLite cannot read is corruption,
        // not an internal error; a missing file takes the init path.
        // Coded lock contention retries through the single bounded
        // wait in `open`; every other failure refuses immediately.
        let fresh = !path.is_file();
        let conn = Connection::open(&path).map_err(|e| {
            busy_or(
                &e,
                if fresh {
                    Error::internal("record file unavailable".to_string())
                } else {
                    Error::rejected(CORRUPT_RECORD_FILE)
                },
            )
        })?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(|e| Error::internal(e.to_string()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| {
                busy_or(
                    &e,
                    if fresh {
                        Error::internal("record file unavailable".to_string())
                    } else {
                        Error::rejected(CORRUPT_RECORD_FILE)
                    },
                )
            })?;
        if fresh {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            tx.execute_batch(SCHEMA)
                .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            let version: i64 = tx
                .query_row("SELECT version FROM record_schema", [], |r| r.get(0))
                .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            if version != 0 {
                return Err(Error::internal("record file initialization diverged"));
            }
            tx.execute(
                "INSERT INTO record_identity(install_id, created) VALUES(?, ?)",
                params![install_id, now()],
            )
            .map_err(|e| {
                // A sibling won the fresh initialization between our
                // version check and this insert — classified by the
                // typed UNIQUE extended code, never by message text.
                if is_lost_init(&e) {
                    Error::internal("record file initialization diverged")
                } else {
                    busy_or(&e, Error::internal(e.to_string()))
                }
            })?;
            tx.execute("UPDATE record_schema SET version=?", [FILE_SCHEMA])
                .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            tx.commit()
                .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| Error::internal(format!("record file permissions refused: {e}")))?;
        } else {
            // A foreign, downgraded or corrupt file refuses here; the
            // operator recovers explicitly (restore from backup, remove
            // after inspection) — the daemon never heals it in place.
            // Table presence is decided by a typed sqlite_master
            // query — sqlite_master itself always exists in a valid
            // database, so an absent record_schema means a sibling is
            // mid-initialization, never a corrupt read. That returns
            // the in-progress signal for the single bounded wait in
            // `open`; a still-table-less file after the wait refuses
            // as corrupt. Coded contention at any read below rides
            // the same wait; anything else refuses at once.
            let schema_present: bool = match conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='record_schema'",
                [],
                |r| r.get::<_, i64>(0),
            ) {
                Ok(count) => count > 0,
                Err(error) => {
                    return Err(busy_or(&error, Error::rejected(CORRUPT_RECORD_FILE)));
                }
            };
            if !schema_present {
                return Err(Error::internal("record file initialization in progress"));
            }
            let version: i64 = conn
                .query_row("SELECT version FROM record_schema", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(|e| busy_or(&e, Error::rejected(CORRUPT_RECORD_FILE)))?;
            if version != FILE_SCHEMA {
                return Err(Error::rejected(
                    "record file schema is unsupported; restore the installation backup or remove the file after inspection",
                ));
            }
            let identity: String = conn
                .query_row("SELECT install_id FROM record_identity", [], |r| r.get(0))
                .map_err(|e| busy_or(&e, Error::rejected(CORRUPT_RECORD_FILE)))?;
            if identity != install_id {
                return Err(Error::rejected(
                    "record file identity differs from the installation; restore the installation backup or remove the file after inspection",
                ));
            }
            // Tighten permissions drifted open by operator handling.
            if let Ok(metadata) = std::fs::metadata(&path) {
                if metadata.permissions().mode() & 0o077 != 0 {
                    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                }
            }
            // CAD-779 CSV import receipts: a table added after FILE_SCHEMA
            // 1 files already exist. The version stays 1 — backup manifests
            // bind it per file — and this idempotent create migrates older
            // files forward without touching their rows. The pending/completed
            // state arrived later still: files whose receipts predate it keep
            // working, with every pre-existing row treated as completed —
            // only the current binary ever wrote those rows, and it wrote
            // final receipts exclusively.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS app_record_csv_imports(\
                 request_id TEXT PRIMARY KEY, context_id TEXT NOT NULL,\
                 preview_token TEXT NOT NULL, result TEXT NOT NULL,\
                 state TEXT NOT NULL, at REAL NOT NULL)",
            )
            .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            // The probe classifies by code: contention retries
            // through the bounded wait instead of misreading BUSY as
            // a missing column and dying on a duplicate-column ALTER.
            let needs_state = match conn.prepare("SELECT state FROM app_record_csv_imports LIMIT 0")
            {
                Ok(_) => false,
                Err(error) if is_contention(&error) => {
                    return Err(Error::internal("record file is busy"));
                }
                Err(_) => true,
            };
            if needs_state {
                conn.execute_batch(
                    "ALTER TABLE app_record_csv_imports ADD COLUMN state TEXT NOT NULL DEFAULT 'complete'",
                )
                .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            }
            // CAD-780 audience tables: segments, exclusion lists,
            // suppressions and frozen audiences. Idempotent forward
            // migration like the CSV receipts above; the version
            // stays 1 and older files gain empty tables.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS app_segments(
                 context_id TEXT NOT NULL, id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK(revision>0),
                 name TEXT NOT NULL, definition TEXT NOT NULL, digest TEXT NOT NULL,
                 created REAL NOT NULL, updated REAL NOT NULL,
                 PRIMARY KEY(context_id, id));
                 CREATE TABLE IF NOT EXISTS app_segment_revisions(
                 context_id TEXT NOT NULL, segment_id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK(revision>0),
                 definition TEXT NOT NULL, digest TEXT NOT NULL,
                 actor TEXT NOT NULL, at REAL NOT NULL,
                 PRIMARY KEY(context_id, segment_id, revision));
                 CREATE TABLE IF NOT EXISTS app_exclusions(
                 context_id TEXT NOT NULL, id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK(revision>0),
                 name TEXT NOT NULL, member_ids TEXT NOT NULL, digest TEXT NOT NULL,
                 created REAL NOT NULL, updated REAL NOT NULL,
                 PRIMARY KEY(context_id, id));
                 CREATE TABLE IF NOT EXISTS app_exclusion_revisions(
                 context_id TEXT NOT NULL, list_id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK(revision>0),
                 member_ids TEXT NOT NULL, digest TEXT NOT NULL,
                 actor TEXT NOT NULL, at REAL NOT NULL,
                 PRIMARY KEY(context_id, list_id, revision));
                 CREATE TABLE IF NOT EXISTS app_suppressions(
                 context_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('email','customer')),
                 key TEXT NOT NULL, reason TEXT NOT NULL, at REAL NOT NULL,
                 PRIMARY KEY(context_id, kind, key));
                 CREATE TABLE IF NOT EXISTS app_audience_freezes(
                 context_id TEXT NOT NULL, freeze_id TEXT NOT NULL,
                 base TEXT NOT NULL, exclusion_list_id TEXT,
                 member_ids TEXT NOT NULL, digest TEXT NOT NULL,
                 max_recipients INTEGER NOT NULL, pins TEXT NOT NULL,
                 created REAL NOT NULL, PRIMARY KEY(context_id, freeze_id))",
            )
            .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            // CAD-782 versioned email content: docs, immutable
            // revisions and assistant proposals. Idempotent forward
            // migration like the audience tables above; the version
            // stays 1 and older files gain empty tables.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS app_content_docs(
                 context_id TEXT NOT NULL, campaign_id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK(revision>0),
                 subject TEXT NOT NULL, preheader TEXT NOT NULL,
                 blocks TEXT NOT NULL, content_digest TEXT NOT NULL,
                 approval_revision INTEGER, approval_digest TEXT,
                 actor TEXT NOT NULL, created REAL NOT NULL, updated REAL NOT NULL,
                 PRIMARY KEY(context_id, campaign_id));
                 CREATE TABLE IF NOT EXISTS app_content_revisions(
                 context_id TEXT NOT NULL, campaign_id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK(revision>0),
                 subject TEXT NOT NULL, preheader TEXT NOT NULL,
                 blocks TEXT NOT NULL, content_digest TEXT NOT NULL,
                 actor TEXT NOT NULL, origin TEXT NOT NULL CHECK(origin IN ('operator','proposal')),
                 proposal_id TEXT, at REAL NOT NULL,
                 PRIMARY KEY(context_id, campaign_id, revision));
                 CREATE TABLE IF NOT EXISTS app_content_proposals(
                 context_id TEXT NOT NULL, proposal_id TEXT NOT NULL,
                 campaign_id TEXT NOT NULL, source_revision INTEGER NOT NULL CHECK(source_revision>=0),
                 subject TEXT NOT NULL, preheader TEXT NOT NULL,
                 blocks TEXT NOT NULL, content_digest TEXT NOT NULL,
                 actor TEXT NOT NULL, origin TEXT NOT NULL DEFAULT 'operator-direct' CHECK(origin IN ('operator-direct','assistant-receipt')),
                 receipt_message TEXT, receipt_agent TEXT, receipt_request TEXT,
                 state TEXT NOT NULL CHECK(state IN ('pending','applied','discarded')),
                 created REAL NOT NULL, decided REAL,
                 PRIMARY KEY(context_id, proposal_id));
                 CREATE INDEX IF NOT EXISTS app_content_proposals_campaign ON app_content_proposals(context_id,campaign_id);
                 CREATE UNIQUE INDEX IF NOT EXISTS app_content_proposal_claim ON app_content_proposals(context_id, receipt_message);
                 CREATE TABLE IF NOT EXISTS app_content_proposal_requests(
                 context_id TEXT NOT NULL, request_id TEXT NOT NULL,
                 campaign_id TEXT NOT NULL, source_revision INTEGER NOT NULL CHECK(source_revision>=0),
                 message_id TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN ('open','used')),
                 used_by TEXT, created REAL NOT NULL, decided REAL,
                 PRIMARY KEY(context_id, request_id));
                 CREATE TABLE IF NOT EXISTS app_sender_bindings(
                 context_id TEXT NOT NULL, binding_id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK(revision>0),
                 sender_name TEXT NOT NULL, sender_address TEXT NOT NULL,
                 unsubscribe_base TEXT NOT NULL, connection_id TEXT,
                 binding_digest TEXT NOT NULL,
                 created REAL NOT NULL, updated REAL NOT NULL,
                 PRIMARY KEY(context_id, binding_id));
                 CREATE TABLE IF NOT EXISTS app_sender_binding_revisions(
                 context_id TEXT NOT NULL, binding_id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK(revision>0),
                 sender_name TEXT NOT NULL, sender_address TEXT NOT NULL,
                 unsubscribe_base TEXT NOT NULL, connection_id TEXT,
                 binding_digest TEXT NOT NULL, actor TEXT NOT NULL, at REAL NOT NULL,
                 PRIMARY KEY(context_id, binding_id, revision))",
            )
            .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            // CAD-782 revision 2: proposals record their origin
            // (`operator-direct`; `assistant-receipt` arrived with
            // the CAD-813 verified handoff). Files created between
            // the two landings gain the column with the only value
            // their rows can carry.
            // The probe classifies by code: contention retries
            // through the bounded wait instead of misreading BUSY as
            // a missing column and dying on a duplicate-column ALTER.
            let needs_origin =
                match conn.prepare("SELECT origin FROM app_content_proposals LIMIT 0") {
                    Ok(_) => false,
                    Err(error) if is_contention(&error) => {
                        return Err(Error::internal("record file is busy"));
                    }
                    Err(_) => true,
                };
            if needs_origin {
                conn.execute_batch(
                    "ALTER TABLE app_content_proposals ADD COLUMN origin TEXT NOT NULL DEFAULT 'operator-direct'",
                )
                .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            }
            // CAD-813: verified assistant proposals carry their
            // turn receipt identity (`receipt_message`,
            // `receipt_agent`, `receipt_request`) beside the origin,
            // one chat message claims one proposal across all ids
            // (unique index; NULL receipts exempt), and operator
            // mints one-time proposal requests (campaign + source
            // stamped per chat message). Older files gain nullable
            // columns; operator-direct rows keep NULLs.
            for column in ["receipt_message", "receipt_agent", "receipt_request"] {
                let probe = format!("SELECT {column} FROM app_content_proposals LIMIT 0");
                let missing = match conn.prepare(&probe) {
                    Ok(_) => false,
                    Err(error) if is_contention(&error) => {
                        return Err(Error::internal("record file is busy"));
                    }
                    Err(_) => true,
                };
                if missing {
                    conn.execute_batch(&format!(
                        "ALTER TABLE app_content_proposals ADD COLUMN {column} TEXT"
                    ))
                    .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
                }
            }
            conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS app_content_proposal_claim ON app_content_proposals(context_id, receipt_message);
                 CREATE TABLE IF NOT EXISTS app_content_proposal_requests(
                 context_id TEXT NOT NULL, request_id TEXT NOT NULL,
                 campaign_id TEXT NOT NULL, source_revision INTEGER NOT NULL CHECK(source_revision>=0),
                 message_id TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN ('open','used')),
                 used_by TEXT, created REAL NOT NULL, decided REAL,
                 PRIMARY KEY(context_id, request_id))",
            )
            .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
            // CAD-786 campaign delivery: test-send evidence, the
            // approved send rows, per-recipient deliveries and the
            // hash-only unsubscribe-token index. Idempotent forward
            // migration like the content tables above; the version
            // stays 1 and older files gain empty tables.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS app_campaign_test_sends(
                 context_id TEXT NOT NULL, campaign_id TEXT NOT NULL,
                 content_digest TEXT NOT NULL, link_digest TEXT NOT NULL,
                 accepted INTEGER NOT NULL, at REAL NOT NULL,
                 PRIMARY KEY(context_id,campaign_id,content_digest,link_digest));
                 CREATE TABLE IF NOT EXISTS app_campaign_sends(
                 context_id TEXT NOT NULL, send_id TEXT NOT NULL,
                 campaign_id TEXT NOT NULL, request_id TEXT NOT NULL,
                 content_revision INTEGER NOT NULL, content_digest TEXT NOT NULL,
                 audience_freeze_id TEXT NOT NULL, audience_digest TEXT NOT NULL,
                 connection_id TEXT NOT NULL, auth_revision INTEGER NOT NULL,
                 link_revision INTEGER NOT NULL, link_digest TEXT NOT NULL,
                 max_recipients INTEGER NOT NULL, send_digest TEXT NOT NULL,
 unsubscribe_origin TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN ('prepared','sending','completed','closed')),
                 close_reason TEXT, created REAL NOT NULL, updated REAL NOT NULL,
                 approved_at REAL,
                 PRIMARY KEY(context_id,send_id),
                 UNIQUE(context_id,request_id));
                 CREATE TABLE IF NOT EXISTS app_campaign_deliveries(
                 context_id TEXT NOT NULL, send_id TEXT NOT NULL,
                 customer_id TEXT NOT NULL, email TEXT NOT NULL,
                 idempotency_key TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN ('queued','submitting','accepted','failed','uncertain','suppressed','closed')),
                 attempts INTEGER NOT NULL DEFAULT 0,
                 smtp_code INTEGER, smtp_message TEXT, reason TEXT, resolved_by TEXT,
                 updated REAL NOT NULL,
                 PRIMARY KEY(context_id,send_id,customer_id),
                 UNIQUE(idempotency_key));
                 CREATE TABLE IF NOT EXISTS app_unsubscribe_tokens(
                 token_hash TEXT PRIMARY KEY,
                 context_id TEXT NOT NULL, customer_id TEXT NOT NULL, send_id TEXT NOT NULL,
                 created REAL NOT NULL)",
            )
            .map_err(|e| busy_or(&e, Error::internal(e.to_string())))?;
        }
        Ok(Self {
            install_id: install_id.to_string(),
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        match self.conn.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let guard = poisoned.into_inner();
                self.conn.clear_poison();
                if !guard.is_autocommit() {
                    let _ = guard.execute_batch("ROLLBACK");
                }
                eprintln!("record store: connection lock poisoned; recovered");
                guard
            }
        }
    }

    fn history_in(conn: &impl super::StoreConn, context: &str, id: &str) -> rusqlite::Result<Vec<Value>> {
        conn.query_vec(
            "SELECT revision,body_digest,actor,at FROM app_record_revisions WHERE context_id=? AND record_id=? ORDER BY revision",
            params![context, id],
            |r| {
                Ok(json!({"revision": r.get::<_, i64>(0)?, "digest": r.get::<_, String>(1)?, "actor": r.get::<_, String>(2)?, "at": r.get::<_, f64>(3)?}))
            },
        )
    }

    fn ids_in(conn: &impl super::StoreConn, sql: &str, params: impl rusqlite::Params) -> Result<Vec<String>> {
        conn.query_vec(sql, params, |r| r.get::<_, String>(0))
            .map_err(|e| Error::internal(e.to_string()))
    }

    fn consent_history_in(conn: &impl super::StoreConn, context: &str, id: &str) -> rusqlite::Result<Value> {
        // Per-channel consent transitions derived from the attributed
        // revision bodies: each entry names the revision, channel,
        // resulting state, actor and time. Unparseable bodies are
        // skipped — integrity of the live row is proven separately.
        let rows = conn.query_vec(
            "SELECT revision,body,actor,at FROM app_record_revisions WHERE context_id=? AND record_id=? ORDER BY revision",
            params![context, id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            },
        )?;
        let mut history = Vec::new();
        let mut email: Option<&str> = None;
        let mut sms: Option<&str> = None;
        for (revision, body, actor, at) in rows {
            let Ok(profile) = serde_json::from_str::<CustomerProfile>(&body) else {
                continue;
            };
            let current_email = profile.consent.email.as_str();
            if email != Some(current_email) {
                email = Some(current_email);
                history.push(json!({"revision": revision, "channel": "email", "state": current_email, "actor": actor, "at": at}));
            }
            let current_sms = profile.consent.sms.as_ref().map(ConsentState::as_str);
            if sms != current_sms {
                sms = current_sms;
                if let Some(state) = current_sms {
                    history.push(json!({"revision": revision, "channel": "sms", "state": state, "actor": actor, "at": at}));
                }
            }
        }
        Ok(Value::Array(history))
    }

    fn show_in(&self, conn: &impl super::StoreConn, context: &str, id: &str) -> Result<Value> {
        let (revision, kind, body, digest): (i64, String, String, String) = conn
            .query_row(
                "SELECT revision,kind,body,body_digest FROM app_records WHERE context_id=? AND id=?",
                params![context, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
            .ok_or_else(|| {
                Error::rejected("record is unavailable for this installation and context")
            })?;
        if kind != "customer" || revision < 1 || body.len() > RECORD_BODY_BYTES {
            return Err(Error::rejected("record integrity refused"));
        }
        let profile: CustomerProfile =
            serde_json::from_str(&body).map_err(|_| Error::rejected("record integrity refused"))?;
        if profile.digest(&self.install_id, context)? != digest {
            return Err(Error::rejected("record integrity refused"));
        }
        let history =
            Self::history_in(conn, context, id).map_err(|e| Error::internal(e.to_string()))?;
        let consent_history = Self::consent_history_in(conn, context, id)
            .map_err(|e| Error::internal(e.to_string()))?;
        Ok(
            json!({"id": id, "install_id": self.install_id, "context_id": context, "kind": kind, "revision": revision, "digest": digest, "profile": profile, "history": history, "consent_history": consent_history}),
        )
    }

    pub fn app_record_create(
        &self,
        context: &str,
        record_id: &str,
        profile: &CustomerProfile,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(record_id, "record ID")?;
        let digest = profile.digest(&self.install_id, context)?;
        let body = serde_json::to_string(profile).map_err(|e| Error::internal(e.to_string()))?;
        // `write_tx` holds `BEGIN IMMEDIATE` across the
        // normalized-email check + writes: a sibling create blocks on
        // the write lock first, so exactly one ID wins an address and
        // the loser is refused with no second live row.
        let result = self.write_tx(|tx| {
            if let Some(existing) = tx
                .query_opt(
                    "SELECT body_digest FROM app_records WHERE context_id=? AND id=?",
                    params![context, record_id],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?
            {
                if existing != digest {
                    return Err(Error::rejected("record ID already holds different content"));
                }
                return Ok(json!({"record": self.show_in(&*tx, context, record_id)?}));
            }
            // One address, one live row per context: a new ID behind a
            // held normalized email is refused, never merged. Profiles
            // without an address skip the check.
            Self::email_conflict_in(&*tx, context, record_id, profile)?;
            let count: i64 = tx
                .query_row(
                    "SELECT count(*) FROM app_records WHERE context_id=?",
                    [context],
                    |r| r.get(0),
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if count >= RECORD_LIMIT {
                return Err(Error::rejected("context has reached its record limit"));
            }
            tx.execute(
                "INSERT INTO app_records(context_id,id,kind,revision,body,body_digest,created,updated) VALUES(?, ?, 'customer', 1, ?, ?, ?, ?)",
                params![context, record_id, body, digest, now(), now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            tx.execute(
                "INSERT INTO app_record_revisions(context_id,record_id,revision,body,body_digest,actor,at) VALUES(?, ?, 1, ?, ?, 'operator', ?)",
                params![context, record_id, body, digest, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok(json!({"record": self.show_in(&*tx, context, record_id)?}))
        })?;
        Ok(result)
    }

    pub fn app_record_show(&self, context: &str, record_id: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(record_id, "record ID")?;
        let conn = self.conn();
        Ok(json!({"record": self.show_in(&conn, context, record_id)?}))
    }

    pub fn app_record_list(&self, context: &str) -> Result<Value> {
        self.app_record_list_paged(context, None, RECORD_LIMIT, None)
    }

    /// Bounded search and pagination over one context's customers.
    /// `query` is a bounded substring matched against the record ID
    /// and body; `limit` is 1..=100; `cursor` pages after a record ID.
    /// The response carries `records`, `truncated` and `next_cursor`
    /// (null when complete) alongside each record's show receipt.
    pub fn app_record_list_paged(
        &self,
        context: &str,
        query: Option<&str>,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        if !(1..=RECORD_LIMIT).contains(&limit) {
            return Err(Error::rejected("record page limit is out of bounds"));
        }
        let like = match query {
            None => None,
            Some(text) => {
                if text.is_empty() || text.len() > 120 || text.chars().any(char::is_control) {
                    return Err(Error::rejected("record search query is out of bounds"));
                }
                let mut escaped = String::with_capacity(text.len() + 2);
                escaped.push('%');
                for ch in text.chars() {
                    if matches!(ch, '\\' | '%' | '_') {
                        escaped.push('\\');
                    }
                    escaped.push(ch);
                }
                escaped.push('%');
                Some(escaped)
            }
        };
        if let Some(after) = cursor {
            crate::proto::identifier(after, "record cursor")?;
        }
        let conn = self.conn();
        let mut sql = String::from("SELECT id FROM app_records WHERE context_id=?");
        if cursor.is_some() {
            sql.push_str(" AND id > ?");
        }
        if like.is_some() {
            sql.push_str(" AND (id LIKE ? ESCAPE '\\' OR body LIKE ? ESCAPE '\\')");
        }
        sql.push_str(" ORDER BY id LIMIT ?");
        let take = limit
            .checked_add(1)
            .ok_or_else(|| Error::rejected("record page limit is out of bounds"))?;
        // Placeholder order follows the SQL above: context, cursor,
        // search patterns, page bound.
        let ids = match (cursor, like.as_deref()) {
            (None, None) => Self::ids_in(&conn, &sql, params![context, take])?,
            (Some(after), None) => Self::ids_in(&conn, &sql, params![context, after, take])?,
            (None, Some(pattern)) => {
                Self::ids_in(&conn, &sql, params![context, pattern, pattern, take])?
            }
            (Some(after), Some(pattern)) => {
                Self::ids_in(&conn, &sql, params![context, after, pattern, pattern, take])?
            }
        };
        let truncated = ids.len() as i64 > limit;
        let page: Vec<String> = ids.into_iter().take(limit as usize).collect();
        let next_cursor = if truncated {
            page.last()
                .cloned()
                .map(Value::String)
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let records = page
            .iter()
            .map(|id| self.show_in(&conn, context, id))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"records": records, "truncated": truncated, "next_cursor": next_cursor}))
    }

    pub fn app_record_update(
        &self,
        context: &str,
        record_id: &str,
        expected: i64,
        profile: &CustomerProfile,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(record_id, "record ID")?;
        if expected < 1 {
            return Err(Error::rejected("expected record revision must be positive"));
        }
        let digest = profile.digest(&self.install_id, context)?;
        let body = serde_json::to_string(profile).map_err(|e| Error::internal(e.to_string()))?;
        // `write_tx` holds `BEGIN IMMEDIATE` across the
        // normalized-email check + update: a racing writer's commit is
        // seen before this lands (IMMEDIATE, like create).
        self.write_tx(|tx| {
            let current = self.show_in(&*tx, context, record_id)?;
            if current["revision"].as_i64() != Some(expected) {
                return Err(Error::rejected("record revision is stale"));
            }
            // A move onto another live row's normalized email is
            // refused, never merged; the row itself is excluded from
            // the check.
            Self::email_conflict_in(&*tx, context, record_id, profile)?;
            let revision = expected
                .checked_add(1)
                .ok_or_else(|| Error::rejected("record revision exhausted"))?;
            let changed = tx
                .execute(
                    "UPDATE app_records SET revision=?, body=?, body_digest=?, updated=? WHERE context_id=? AND id=? AND revision=?",
                    params![revision, body, digest, now(), context, record_id, expected],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if changed != 1 {
                return Err(Error::rejected("record revision is stale"));
            }
            tx.execute(
                "INSERT INTO app_record_revisions(context_id,record_id,revision,body,body_digest,actor,at) VALUES(?, ?, ?, ?, ?, 'operator', ?)",
                params![context, record_id, revision, body, digest, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            Ok(json!({"record": self.show_in(&*tx, context, record_id)?}))
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentState {
    Granted,
    Denied,
    Unknown,
}

impl ConsentState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomerConsent {
    pub email: ConsentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sms: Option<ConsentState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomerProfile {
    pub schema: u32,
    pub display_name: String,
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub consent: CustomerConsent,
}

/// The only profile refusal: a unit struct, so a wrong value can
/// never echo private data into logs or error frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileRefused;
impl std::fmt::Display for ProfileRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("record profile exceeds its supported shape or bounds")
    }
}

/// Bounded single-@ shape check only; deliverability is a later
/// slice's problem. Shared by profile validation and CSV row
/// classification so both refuse exactly the same addresses.
pub fn email_shape_valid(email: &str) -> bool {
    let mut parts = email.split('@');
    email.len() <= 254
        && !email
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        && matches!((parts.next(), parts.next(), parts.next()), (Some(user), Some(domain), None) if !user.is_empty() && domain.contains('.') && !domain.is_empty())
}

/// Bounded telephone shape: diallable characters with at least seven
/// digits. Formatting is preserved verbatim; validity is shape only.
pub fn phone_shape_valid(phone: &str) -> bool {
    phone.len() >= 7
        && phone.len() <= 24
        && phone.bytes().filter(|b| b.is_ascii_digit()).count() >= 7
        && phone
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b' ' | b'-' | b'(' | b')' | b'.'))
}

/// Bounded record-source label: the same tag grammar as `tags`.
pub fn source_shape_valid(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 40
        && source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Bounded display-name shape shared by profile validation and CSV
/// row classification so both refuse exactly the same names.
pub fn display_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

/// Bounded tag shape shared by profile validation and CSV row
/// classification.
pub fn tag_valid(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 40
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

impl CustomerProfile {
    /// Parse an untrusted profile value. Refusals name the shape, never
    /// the content — a wrong value must not echo private data into
    /// logs or error frames.
    pub fn parse(body: &Value) -> std::result::Result<Self, ProfileRefused> {
        let profile: Self = serde_json::from_value(body.clone()).map_err(|_| ProfileRefused)?;
        profile.validate().map_err(|_| ProfileRefused)?;
        Ok(profile)
    }

    fn validate(&self) -> Result<()> {
        if self.schema != 1 || !display_name_valid(&self.display_name) {
            return Err(Error::rejected(
                "record profile exceeds its supported shape or bounds",
            ));
        }
        if let Some(email) = &self.email {
            // Bounded single-@ shape check only; deliverability is a
            // later slice's problem. No content is echoed on refusal.
            if !email_shape_valid(email) {
                return Err(Error::rejected(
                    "record profile exceeds its supported shape or bounds",
                ));
            }
        }
        if let Some(phone) = &self.phone {
            if !phone_shape_valid(phone) {
                return Err(Error::rejected(
                    "record profile exceeds its supported shape or bounds",
                ));
            }
        }
        if let Some(source) = &self.source {
            if !source_shape_valid(source) {
                return Err(Error::rejected(
                    "record profile exceeds its supported shape or bounds",
                ));
            }
        }
        if self.tags.len() > 16 || self.tags.iter().any(|tag| !tag_valid(tag)) {
            return Err(Error::rejected(
                "record profile exceeds its supported shape or bounds",
            ));
        }
        let mut sorted = self.tags.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != self.tags.len() {
            return Err(Error::rejected(
                "record profile exceeds its supported shape or bounds",
            ));
        }
        if serde_json::to_vec(self)
            .map_err(|e| Error::internal(e.to_string()))?
            .len()
            > RECORD_BODY_BYTES
        {
            return Err(Error::rejected(
                "record profile exceeds its supported shape or bounds",
            ));
        }
        Ok(())
    }

    fn digest(&self, install_id: &str, context_id: &str) -> Result<String> {
        self.validate()?;
        Ok(material_digest(
            &json!({"domain":"cadence-app-record-v1","install_id":install_id,"context_id":context_id,"kind":"customer","profile":self}),
        ))
    }
}

/// CAD-779 CSV preview/import over the installation record file.
///
/// Preview parses bounded CSV text into per-row create/update/skip
/// decisions without mutating anything; the returned preview token
/// binds the exact bytes. Import reserves the operator request id as
/// a pending receipt before any row mutates, applies each row in its
/// own expected-revision transaction, then completes the receipt with
/// the per-row outcome — so a receipt failure can never leave rows
/// behind, and retries replay the stored receipt. A pending receipt
/// means a sibling is applying: same bytes wait for completion via
/// retry, different bytes refuse at once. Recovery after a stuck
/// pending row is a fresh request id, whose re-plan converges on
/// skips for already-applied rows. Consent is never inferred: absent
/// consent cells arrive as unknown. Row refusals name a code, never
/// the cell.
pub const CSV_TEXT_BYTES: usize = 256 * 1024;
pub const CSV_ROWS_MAX: usize = 500;
const CSV_COLUMNS: &[&str] = &[
    "record_id",
    "display_name",
    "email",
    "phone",
    "tags",
    "source",
    "consent_email",
    "consent_sms",
    "expected_revision",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CsvAction {
    Create,
    Update,
    Skip,
}

#[derive(Clone, Debug)]
pub struct CsvDecision {
    pub row: i64,
    pub action: CsvAction,
    pub expected_revision: Option<i64>,
}

struct PlanRow {
    number: i64,
    record_id: String,
    profile: Option<CustomerProfile>,
    expected_revision: Option<i64>,
    current_revision: Option<i64>,
    decision: &'static str,
    errors: Vec<&'static str>,
    reason: Option<&'static str>,
    duplicate_of: Option<String>,
}

/// Strict field splitter: commas separate, double quotes group with
/// `""` escapes, lines end at `\n` or `\r\n`. Anything else —
/// lone carriage returns, control bytes, unterminated quotes,
/// trailing content after a closing quote — refuses the whole file
/// as corrupt. No cell value ever enters the refusal.
fn csv_cells(text: &str) -> Result<Vec<Vec<String>>> {
    const CORRUPT: &str = "customer CSV is corrupt";
    if text
        .bytes()
        .any(|b| b == 0 || (b.is_ascii_control() && !matches!(b, b'\n' | b'\r' | b'\t')))
    {
        return Err(Error::rejected(CORRUPT));
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut closed = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                    closed = true;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !closed => {
                in_quotes = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                closed = false;
            }
            '\n' => {
                record.push(std::mem::take(&mut field));
                closed = false;
                rows.push(std::mem::take(&mut record));
            }
            '\r' => {
                if chars.next() != Some('\n') {
                    return Err(Error::rejected(CORRUPT));
                }
                record.push(std::mem::take(&mut field));
                closed = false;
                rows.push(std::mem::take(&mut record));
            }
            _ => {
                if closed {
                    return Err(Error::rejected(CORRUPT));
                }
                field.push(c);
            }
        }
    }
    if in_quotes {
        return Err(Error::rejected(CORRUPT));
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        rows.push(record);
    }
    if rows.is_empty() {
        return Err(Error::rejected("customer CSV is empty"));
    }
    Ok(rows)
}

/// Classify a refused profile from its cells without echoing them.
/// Every code is a fixed string; predicates mirror `validate`.
fn classify_profile_cells(
    display: &str,
    email: &str,
    phone: &str,
    tags: &str,
    source: &str,
    consent_email: &str,
    consent_sms: &str,
) -> &'static str {
    if !display_name_valid(display) {
        return "invalid display name";
    }
    if !email.is_empty() && !email_shape_valid(email) {
        return "invalid email";
    }
    if !phone.is_empty() && !phone_shape_valid(phone) {
        return "invalid phone";
    }
    if !tags.is_empty()
        && (tags.split(';').any(|tag| !tag_valid(tag)) || tags.split(';').count() > 16)
    {
        return "invalid tags";
    }
    if !source.is_empty() && !source_shape_valid(source) {
        return "invalid source";
    }
    for cell in [consent_email, consent_sms] {
        if !cell.is_empty() && !matches!(cell, "granted" | "denied" | "unknown") {
            return "invalid consent";
        }
    }
    "invalid profile"
}

fn plan_row_json(row: &PlanRow) -> Value {
    let mut receipt = json!({
        "row": row.number,
        "record_id": row.record_id,
        "decision": row.decision,
        "expected_revision": row.expected_revision,
        "current_revision": row.current_revision,
        "profile": row.profile.as_ref().map(|profile| serde_json::to_value(profile).unwrap_or(Value::Null)).unwrap_or(Value::Null),
        "errors": row.errors,
        "reason": row.reason,
    });
    // A row with no duplicate candidate carries no duplicate key at
    // all: absence — not null — is the isolation proof's signal.
    if let Some(other) = &row.duplicate_of {
        receipt["duplicate_of"] = Value::String(other.clone());
    }
    receipt
}

impl RecordStore {
    /// Normalized-email conflict check inside an open write
    /// transaction: any OTHER live row in this context holding the
    /// profile's lowered address refuses the write. The caller holds
    /// an IMMEDIATE transaction, so a sibling writer blocks on the
    /// write lock and commits first — then this read sees it.
    fn email_conflict_in(
        tx: &rusqlite::Transaction<'_>,
        context: &str,
        record_id: &str,
        profile: &CustomerProfile,
    ) -> Result<()> {
        let Some(address) = profile.email.as_deref().map(str::to_lowercase) else {
            return Ok(());
        };
        let mut stmt = tx
            .prepare("SELECT id,body FROM app_records WHERE context_id=?")
            .map_err(|e| Error::internal(e.to_string()))?;
        let found = stmt
            .query_map([context], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| Error::internal(e.to_string()))?;
        for row in found {
            let (id, body) = row.map_err(|e| Error::internal(e.to_string()))?;
            if id == record_id {
                continue;
            }
            let stored: Value = serde_json::from_str(&body).map_err(|_| {
                Error::internal(
                    "record profile is corrupt; restore the installation backup after inspection",
                )
            })?;
            let stored = CustomerProfile::parse(&stored).map_err(|_| {
                Error::internal(
                    "record profile is corrupt; restore the installation backup after inspection",
                )
            })?;
            let held = stored.email.as_deref().map(str::to_lowercase);
            if held.as_deref() == Some(address.as_str()) {
                return Err(Error::rejected(
                    "record email is already used by another record",
                ));
            }
        }
        Ok(())
    }

    fn import_receipt_in(
        conn: &impl super::StoreConn,
        request_id: &str,
    ) -> Result<Option<(String, String, String, String)>> {
        conn.query_row(
            "SELECT context_id,preview_token,result,state FROM app_record_csv_imports WHERE request_id=?",
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    /// Settle a request id against its stored receipt: a completed
    /// receipt with matching bytes replays verbatim; a pending row
    /// with matching bytes means a sibling is applying now, so refuse
    /// and let the caller retry into the completed receipt — rows are
    /// never applied twice under one id. Any id reuse behind different
    /// bytes refuses before a single row mutates.
    fn replay_or_refuse(
        stored: (String, String, String, String),
        context: &str,
        preview_token: &str,
    ) -> Result<Value> {
        if stored.0 == context && stored.1 == preview_token {
            if stored.3.as_str() == "complete" {
                let mut result: Value = serde_json::from_str(&stored.2)
                    .map_err(|_| Error::rejected("customer CSV receipt is unavailable"))?;
                result["replayed"] = Value::Bool(true);
                return Ok(result);
            }
            return Err(Error::rejected(
                "customer CSV import is already in progress",
            ));
        }
        Err(Error::rejected("customer CSV request id is already used"))
    }

    /// Parse bounded CSV text into a bound preview token plus one
    /// planned row per data line. Reads the context's live rows for
    /// duplicate and revision comparison; writes nothing.
    fn plan_csv_in(
        &self,
        conn: &impl super::StoreConn,
        context: &str,
        csv_text: &str,
    ) -> Result<(String, Vec<PlanRow>)> {
        if csv_text.is_empty() {
            return Err(Error::rejected("customer CSV is empty"));
        }
        if csv_text.len() > CSV_TEXT_BYTES {
            return Err(Error::rejected("customer CSV exceeds its size bound"));
        }
        let table = csv_cells(csv_text)?;
        let header = &table[0];
        if header.len() > 16 {
            return Err(Error::rejected("customer CSV has unsupported columns"));
        }
        for name in header {
            if !CSV_COLUMNS.contains(&name.as_str()) {
                return Err(Error::rejected("customer CSV has unsupported columns"));
            }
        }
        {
            let mut sorted = header.clone();
            sorted.sort();
            sorted.dedup();
            if sorted.len() != header.len() {
                return Err(Error::rejected("customer CSV has duplicate columns"));
            }
        }
        if !header.iter().any(|name| name == "display_name") {
            return Err(Error::rejected(
                "customer CSV requires a display name column",
            ));
        }
        let index = |column: &str| header.iter().position(|name| name == column);
        let mut data: Vec<(i64, Vec<String>)> = Vec::new();
        for fields in table.iter().skip(1) {
            if fields.iter().all(|field| field.is_empty()) {
                continue;
            }
            data.push((data.len() as i64 + 1, fields.clone()));
        }
        if data.len() > CSV_ROWS_MAX {
            return Err(Error::rejected("customer CSV exceeds its row bound"));
        }
        // Cell-level parse first; every refusal below is a fixed code.
        struct Raw {
            number: i64,
            record_id: String,
            display: String,
            email: String,
            phone: String,
            tags: String,
            source: String,
            consent_email: String,
            consent_sms: String,
            sms_column: bool,
            expected_revision: Option<i64>,
            errors: Vec<&'static str>,
        }
        let cell = |fields: &[String], column: &str| -> String {
            index(column)
                .and_then(|at| fields.get(at))
                .cloned()
                .unwrap_or_default()
        };
        let mut raws: Vec<Raw> = Vec::with_capacity(data.len());
        for (number, fields) in &data {
            let mut errors: Vec<&'static str> = Vec::new();
            let record_id = cell(fields, "record_id");
            if fields.len() != header.len() {
                errors.push("column count");
            }
            if record_id.is_empty() {
                // Blank IDs are refused, never derived: a
                // deterministic per-file id would collide across
                // installations reusing the same import.
                errors.push("record id");
            } else if crate::proto::identifier(&record_id, "record ID").is_err() {
                errors.push("record id");
            }
            let revision_cell = cell(fields, "expected_revision");
            let expected_revision = if revision_cell.is_empty() {
                None
            } else {
                match revision_cell.parse::<i64>() {
                    Ok(revision) if revision > 0 => Some(revision),
                    _ => {
                        errors.push("expected revision");
                        None
                    }
                }
            };
            raws.push(Raw {
                number: *number,
                record_id,
                display: cell(fields, "display_name"),
                email: cell(fields, "email"),
                phone: cell(fields, "phone"),
                tags: cell(fields, "tags"),
                source: cell(fields, "source"),
                consent_email: cell(fields, "consent_email"),
                consent_sms: cell(fields, "consent_sms"),
                sms_column: index("consent_sms").is_some(),
                expected_revision,
                errors,
            });
        }
        // Profile shape next, from cells — never echoed.
        let mut profiles: Vec<Option<CustomerProfile>> = Vec::with_capacity(raws.len());
        for raw in &raws {
            if !raw.errors.is_empty() {
                profiles.push(None);
                continue;
            }
            let tags: Vec<String> = if raw.tags.is_empty() {
                Vec::new()
            } else {
                raw.tags.split(';').map(str::to_string).collect()
            };
            let consent_email = if raw.consent_email.is_empty() {
                // Consent is never inferred from import: an absent
                // cell arrives as unknown, never granted.
                "unknown"
            } else {
                raw.consent_email.as_str()
            };
            let mut consent = json!({"email": consent_email});
            if raw.sms_column {
                consent["sms"] = Value::String(if raw.consent_sms.is_empty() {
                    "unknown".to_string()
                } else {
                    raw.consent_sms.clone()
                });
            }
            let mut body =
                json!({"schema": 1, "display_name": raw.display, "tags": tags, "consent": consent});
            if !raw.email.is_empty() {
                body["email"] = Value::String(raw.email.clone());
            }
            if !raw.phone.is_empty() {
                body["phone"] = Value::String(raw.phone.clone());
            }
            if !raw.source.is_empty() {
                body["source"] = Value::String(raw.source.clone());
            }
            match CustomerProfile::parse(&body) {
                Ok(profile) => profiles.push(Some(profile)),
                Err(_) => profiles.push(None),
            }
        }
        // Within-file duplicates error every row sharing the key.
        let mut id_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut email_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for (at, raw) in raws.iter().enumerate() {
            if raw.errors.contains(&"record id") {
                continue;
            }
            if profiles[at].is_some() {
                *id_counts.entry(raw.record_id.as_str()).or_insert(0) += 1;
            }
            if !raw.email.is_empty() {
                *email_counts.entry(raw.email.to_lowercase()).or_insert(0) += 1;
            }
        }
        // Live rows for duplicate and revision comparison.
        let mut by_id: std::collections::HashMap<String, (i64, String)> =
            std::collections::HashMap::new();
        let mut by_email: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT id,revision,body_digest,body FROM app_records WHERE context_id=?")
                .map_err(|e| Error::internal(e.to_string()))?;
            let found = stmt
                .query_map([context], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(|e| Error::internal(e.to_string()))?;
            for row in found {
                let (id, revision, digest, body) =
                    row.map_err(|e| Error::internal(e.to_string()))?;
                let email = serde_json::from_str::<Value>(&body).ok().and_then(|value| {
                    value
                        .get("email")
                        .and_then(Value::as_str)
                        .map(str::to_lowercase)
                });
                if let Some(address) = email {
                    by_email.entry(address).or_insert_with(|| id.clone());
                }
                by_id.insert(id, (revision, digest));
            }
        }
        let mut plan: Vec<PlanRow> = Vec::with_capacity(raws.len());
        for (at, raw) in raws.iter().enumerate() {
            let mut errors = raw.errors.clone();
            let mut duplicate_of: Option<String> = None;
            if errors.is_empty() && profiles[at].is_none() {
                errors.push(classify_profile_cells(
                    &raw.display,
                    &raw.email,
                    &raw.phone,
                    &raw.tags,
                    &raw.source,
                    &raw.consent_email,
                    &raw.consent_sms,
                ));
            }
            if errors.is_empty()
                && id_counts
                    .get(raw.record_id.as_str())
                    .is_some_and(|count| *count > 1)
            {
                errors.push("duplicate record");
            }
            if errors.is_empty()
                && !raw.email.is_empty()
                && email_counts
                    .get(&raw.email.to_lowercase())
                    .is_some_and(|count| *count > 1)
            {
                errors.push("duplicate email");
            }
            let mut decision = "error";
            let mut reason: Option<&'static str> = None;
            let mut current_revision: Option<i64> = None;
            if errors.is_empty() {
                let profile = profiles[at]
                    .as_ref()
                    .ok_or_else(|| Error::internal("customer CSV plan diverged"))?;
                match by_id.get(&raw.record_id) {
                    Some((revision, digest)) => {
                        current_revision = Some(*revision);
                        let fresh = profile.digest(&self.install_id, context)?;
                        if fresh == *digest {
                            decision = "skip";
                            reason = Some("already current");
                        } else if raw.expected_revision.is_none() {
                            // The operator must name the revision they
                            // saw: a changed row never updates blind.
                            decision = "needs_revision";
                            reason = Some("missing expected revision");
                        } else {
                            // Revision match is advisory here; the
                            // import re-checks CAS live at apply time.
                            decision = "update";
                        }
                    }
                    None => {
                        if !raw.email.is_empty() {
                            if let Some(other) = by_email.get(&raw.email.to_lowercase()) {
                                // Never auto-merge: a new id behind a
                                // known address waits for the operator.
                                errors.push("duplicate email");
                                duplicate_of = Some(other.clone());
                            }
                        }
                        if errors.is_empty() {
                            decision = "create";
                        }
                    }
                }
            }
            plan.push(PlanRow {
                number: raw.number,
                record_id: raw.record_id.clone(),
                profile: if errors.is_empty() {
                    profiles[at].clone()
                } else {
                    None
                },
                expected_revision: raw.expected_revision,
                current_revision,
                decision,
                errors,
                reason,
                duplicate_of,
            });
        }
        let token = material_digest(
            &json!({"domain":"cadence-app-record-csv-preview-v1","install_id":self.install_id,"context_id":context,"csv":csv_text}),
        );
        Ok((token, plan))
    }

    /// Read-only plan of bounded CSV text: per-row decisions plus the
    /// preview token that binds the exact bytes. Writes nothing.
    pub fn app_record_csv_preview(&self, context: &str, csv_text: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let (token, plan) = self.plan_csv_in(&conn, context, csv_text)?;
        let mut create = 0;
        let mut update = 0;
        let mut skip = 0;
        let mut needs_revision = 0;
        let mut error = 0;
        for row in &plan {
            match row.decision {
                "create" => create += 1,
                "update" => update += 1,
                "skip" => skip += 1,
                "needs_revision" => needs_revision += 1,
                _ => error += 1,
            }
        }
        Ok(json!({
            "preview_token": token,
            "row_count": plan.len(),
            "summary": {"create": create, "update": update, "skip": skip, "needs_revision": needs_revision, "error": error},
            "rows": plan.iter().map(plan_row_json).collect::<Vec<_>>(),
        }))
    }

    /// Explicit import of a previewed CSV. The token must bind these
    /// exact bytes; `request_id` is reserved as a pending receipt before
    /// any row mutates. A repeat with the same bytes replays the stored
    /// receipt; a pending same-bytes sibling refuses as in progress so
    /// the caller retries into completion; a reused id behind different
    /// bytes refuses before any row mutates. Each row commits in its own
    /// transaction, so malformed rows and stale writes can never lose an
    /// existing record; the completed per-row receipt is the recoverable
    /// record.
    pub fn app_record_csv_import(
        &self,
        context: &str,
        csv_text: &str,
        preview_token: &str,
        request_id: &str,
        decisions: Option<Vec<CsvDecision>>,
    ) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(request_id, "request ID")?;
        if preview_token.len() > 128 || !preview_token.starts_with("sha256:") {
            return Err(Error::rejected(
                "customer CSV preview token is out of bounds",
            ));
        }
        let ordered: Vec<CsvDecision> = match decisions {
            None => Vec::new(),
            Some(list) => {
                if list.len() > CSV_ROWS_MAX {
                    return Err(Error::rejected("customer CSV decisions exceed their bound"));
                }
                let mut seen = std::collections::HashSet::new();
                for item in &list {
                    if item.row < 1 {
                        return Err(Error::rejected(
                            "customer CSV decision rows must be positive",
                        ));
                    }
                    if item.expected_revision.is_some_and(|revision| revision < 1) {
                        return Err(Error::rejected("expected record revision must be positive"));
                    }
                    if !seen.insert(item.row) {
                        return Err(Error::rejected("customer CSV decision rows must be unique"));
                    }
                }
                list
            }
        };
        // A known request id settles against its stored receipt
        // before anything is planned or mutated.
        {
            let conn = self.conn();
            if let Some(stored) = Self::import_receipt_in(&conn, request_id)? {
                return Self::replay_or_refuse(stored, context, preview_token);
            }
        }
        let (token, plan) = {
            let conn = self.conn();
            self.plan_csv_in(&conn, context, csv_text)?
        };
        if token != preview_token {
            return Err(Error::rejected(
                "customer CSV preview is stale; preview again",
            ));
        }
        for item in &ordered {
            if (item.row as usize) > plan.len() {
                return Err(Error::rejected(
                    "customer CSV decision targets an unknown row",
                ));
            }
        }
        enum Apply {
            Create(CustomerProfile),
            Update(CustomerProfile, i64),
            Skip(&'static str),
        }
        let planned = |row: &PlanRow| -> Result<CustomerProfile> {
            row.profile
                .clone()
                .ok_or_else(|| Error::internal("customer CSV plan diverged"))
        };
        let by_row: std::collections::HashMap<i64, &CsvDecision> =
            ordered.iter().map(|item| (item.row, item)).collect();
        let mut applies: Vec<Apply> = Vec::with_capacity(plan.len());
        for row in &plan {
            let apply = match by_row.get(&row.number) {
                None => match row.decision {
                    "create" => Apply::Create(planned(row)?),
                    "update" => Apply::Update(
                        planned(row)?,
                        row.expected_revision
                            .ok_or_else(|| Error::internal("customer CSV plan diverged"))?,
                    ),
                    _ if row.decision == "error" => Apply::Skip("row error"),
                    _ => Apply::Skip(row.reason.unwrap_or("skipped")),
                },
                Some(item) => {
                    if row.decision == "error" {
                        return Err(Error::rejected(
                            "customer CSV decision targets an error row",
                        ));
                    }
                    match item.action {
                        CsvAction::Skip => Apply::Skip("operator skipped"),
                        CsvAction::Create => {
                            if row.decision != "create" {
                                return Err(Error::rejected(
                                    "customer CSV decision does not match the preview plan",
                                ));
                            }
                            Apply::Create(planned(row)?)
                        }
                        CsvAction::Update => {
                            if row.decision != "update" && row.decision != "needs_revision" {
                                return Err(Error::rejected(
                                    "customer CSV decision does not match the preview plan",
                                ));
                            }
                            let revision = item
                                .expected_revision
                                .or(row.expected_revision)
                                .ok_or_else(|| {
                                    Error::rejected(
                                        "customer CSV update needs an expected revision",
                                    )
                                })?;
                            Apply::Update(planned(row)?, revision)
                        }
                    }
                }
            };
            applies.push(apply);
        }
        // Validate every decision before reserving the id: a malformed
        // request must not leave a pending receipt that blocks a retry.
        // Then reserve before any row mutates. A lost reservation race
        // settles against the winner's row: completed replays, pending
        // or foreign refuses; a genuine write failure leaves rows untouched.
        {
            let conn = self.conn();
            match conn.execute(
                "INSERT INTO app_record_csv_imports(request_id,context_id,preview_token,result,state,at) VALUES(?,?,?,?,?,?)",
                params![request_id, context, token, "", "pending", now()],
            ) {
                Ok(_) => {}
                Err(_) => match Self::import_receipt_in(&conn, request_id)? {
                    Some(winner) if winner.0 == context && winner.1 == preview_token => {
                        return Self::replay_or_refuse(winner, context, preview_token);
                    }
                    Some(_) => {
                        return Err(Error::rejected(
                            "customer CSV request id is already used",
                        ));
                    }
                    None => {
                        return Err(Error::internal("customer CSV receipt write refused"));
                    }
                },
            }
        }
        // Per-row transactions: a failure records its row, never the file.
        let refused = |error: &Error| -> &'static str {
            let text = error.to_string();
            if text.contains("stale") {
                "stale revision"
            } else if text.contains("another record") {
                "duplicate email"
            } else if text.contains("already holds") {
                "record conflict"
            } else {
                "record refused"
            }
        };
        let mut applied = 0;
        let mut skipped = 0;
        let mut failed = 0;
        let mut outcomes: Vec<Value> = Vec::with_capacity(plan.len());
        for (row, apply) in plan.iter().zip(applies.iter()) {
            let (outcome, reason) = match apply {
                Apply::Skip(why) => {
                    skipped += 1;
                    ("skipped", Some(*why))
                }
                Apply::Create(profile) => {
                    let stored = self.app_record_create(context, &row.record_id, profile);
                    match stored {
                        Ok(_) => {
                            applied += 1;
                            ("created", None)
                        }
                        Err(error) => {
                            failed += 1;
                            ("failed", Some(refused(&error)))
                        }
                    }
                }
                Apply::Update(profile, revision) => {
                    let stored =
                        self.app_record_update(context, &row.record_id, *revision, profile);
                    match stored {
                        Ok(_) => {
                            applied += 1;
                            ("updated", None)
                        }
                        Err(error) => {
                            failed += 1;
                            ("failed", Some(refused(&error)))
                        }
                    }
                }
            };
            outcomes.push(json!({"row": row.number, "record_id": row.record_id, "outcome": outcome, "reason": reason}));
        }
        let result = json!({
            "request_id": request_id,
            "preview_token": token,
            "replayed": false,
            "summary": {"applied": applied, "skipped": skipped, "failed": failed},
            "rows": outcomes,
        });
        // Complete the reserved receipt with the per-row outcome.
        // Rows are committed; only their attribution was pending.
        {
            let conn = self.conn();
            let stored = result.to_string();
            let completed = conn
                .execute(
                    "UPDATE app_record_csv_imports SET result=?, state='complete', at=? WHERE request_id=? AND state='pending'",
                    params![stored, now(), request_id],
                )
                .map_err(|e| Error::internal(e.to_string()))?;
            if completed != 1 {
                // The reservation vanished mid-import: rows stand but
                // unattributable. Fail loudly, never silently.
                return Err(Error::internal("customer CSV receipt write refused"));
            }
        }
        Ok(result)
    }
}

impl Store {
    /// Best-effort audit event for a record write that already
    /// committed inside its installation file. The event is advisory:
    /// a failure here is logged, never reported as a write failure,
    /// because the record stands either way.
    pub fn note_app_record(
        &self,
        install: &str,
        context: &str,
        record: &str,
        revision: i64,
        digest: &str,
        created: bool,
    ) {
        let _ = self.write_tx(|tx| Self::event(&*tx, Self::DAEMON_STREAM,
            if created {
                "app_record_created"
            } else {
                "app_record_updated"
            },
            json!({"record_id": record, "install_id": install, "context_id": context, "revision": revision, "digest": digest, "actor": "operator"}),));
    }

    /// Best-effort audit event for a CSV import that already committed
    /// its per-row transactions. Counts only — no customer content.
    /// Advisory like `note_app_record`: a failure here is logged,
    /// never reported as an import failure.
    pub fn note_app_record_csv_import(
        &self,
        install: &str,
        context: &str,
        request: &str,
        applied: i64,
        skipped: i64,
        failed: i64,
    ) {
        let _ = self.write_tx(|tx| Self::event(&*tx, Self::DAEMON_STREAM,
            "app_record_csv_imported",
            json!({"install_id": install, "context_id": context, "request_id": request, "applied": applied, "skipped": skipped, "failed": failed, "actor": "operator"}),));
    }
}

#[cfg(test)]
mod open_tests {
    use super::*;

    fn sqlite_failure(code: std::os::raw::c_int) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    #[test]
    fn cad780_open_classifies_contention_by_code_not_text() {
        // SQLITE_BUSY (5) and SQLITE_LOCKED (6) — including extended
        // BUSY_SNAPSHOT and LOCKED_SHAREDCACHE variants — are
        // contention whatever their message says.
        for code in [5, 6, 5 | (2 << 8), 6 | (3 << 8)] {
            assert!(
                is_contention(&sqlite_failure(code)),
                "code {code} not classified as contention"
            );
        }
        // Generic failures, constraint violations and corrupt-page
        // reports are never contention.
        for code in [1, 8, 11, 19] {
            assert!(
                !is_contention(&sqlite_failure(code)),
                "code {code} misclassified as contention"
            );
        }
        assert!(!is_contention(&rusqlite::Error::QueryReturnedNoRows));
        // Exactly the three retry signals are transient; genuine
        // refusals fail closed on the first attempt.
        for signal in [
            "record file initialization diverged",
            "record file initialization in progress",
            "record file is busy",
        ] {
            assert!(
                is_transient_open_error(&Error::internal(signal)),
                "{signal} must retry"
            );
        }
        for refusal in [
            "record file is corrupt or foreign; restore the installation backup or remove the file after inspection",
            "record file schema is unsupported; restore the installation backup or remove the file after inspection",
            "record file identity differs from the installation; restore the installation backup or remove the file after inspection",
            "record file unavailable",
        ] {
            assert!(
                !is_transient_open_error(&Error::internal(refusal)),
                "{refusal} must refuse at once"
            );
        }
        // The contention map preserves the existing refusal for
        // non-contention errors and substitutes the retry signal.
        let busy = busy_or(
            &sqlite_failure(5),
            Error::rejected("record file is corrupt or foreign"),
        );
        assert!(is_transient_open_error(&busy));
        let corrupt = busy_or(
            &sqlite_failure(11),
            Error::rejected("record file is corrupt or foreign"),
        );
        assert!(!is_transient_open_error(&corrupt));
    }

    #[test]
    fn cad780_open_classifies_lost_init_by_extended_code() {
        // The real race, pinned live: a duplicate install_id insert
        // against the exact identity schema reports PRIMARYKEY.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE record_identity(install_id TEXT PRIMARY KEY, created REAL NOT NULL);
             INSERT INTO record_identity(install_id, created) VALUES('install-a', 0.0);",
        )
        .unwrap();
        let raced = conn
            .execute(
                "INSERT INTO record_identity(install_id, created) VALUES('install-a', 0.0)",
                [],
            )
            .unwrap_err();
        assert_eq!(
            raced.sqlite_error().map(|detail| detail.extended_code),
            Some(rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY)
        );
        assert!(is_lost_init(&raced));
        // Sibling constraint codes are not a lost init race —
        // including UNIQUE, which this schema never emits here.
        for code in [
            rusqlite::ffi::SQLITE_CONSTRAINT,
            rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
            rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL,
        ] {
            assert!(
                !is_lost_init(&sqlite_failure(code)),
                "code {code} misclassified as a lost init race"
            );
        }
        // A message that merely mentions uniqueness with a generic
        // code must not route to the init-race retry.
        let decoy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(1),
            Some("UNIQUE constraint failed: record_identity.install_id".to_string()),
        );
        assert!(!is_lost_init(&decoy));
    }
}
