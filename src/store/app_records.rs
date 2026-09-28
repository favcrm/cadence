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

pub const RECORDS_DIR: &str = "app-records";
const FILE_SCHEMA: i64 = 1;

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

/// One installation's record file. Opened per operator action and
/// closed after it — persistence is the file itself, shared through
/// SQLite locking, never daemon memory.
pub struct RecordStore {
    install_id: String,
    conn: Mutex<Connection>,
}

impl RecordStore {
    /// Open (creating and initializing when missing) the
    /// installation's record file. A present file whose schema or
    /// identity row does not match is refused with an explicit
    /// recovery error — user data is never deleted or rewritten.
    pub fn open(state_dir: &Path, install_id: &str) -> Result<Self> {
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
        const CORRUPT: &str = "record file is corrupt or foreign; restore the installation backup or remove the file after inspection";
        let fresh = !path.is_file();
        let conn = Connection::open(&path).map_err(|_| {
            if fresh {
                Error::internal("record file unavailable".to_string())
            } else {
                Error::rejected(CORRUPT)
            }
        })?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(|e| Error::internal(e.to_string()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|_| {
                if fresh {
                    Error::internal("record file unavailable".to_string())
                } else {
                    Error::rejected(CORRUPT)
                }
            })?;
        if fresh {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| Error::internal(e.to_string()))?;
            tx.execute_batch(SCHEMA)
                .map_err(|e| Error::internal(e.to_string()))?;
            let version: i64 = tx
                .query_row("SELECT version FROM record_schema", [], |r| r.get(0))
                .map_err(|e| Error::internal(e.to_string()))?;
            if version != 0 {
                return Err(Error::internal("record file initialization diverged"));
            }
            tx.execute(
                "INSERT INTO record_identity(install_id, created) VALUES(?, ?)",
                params![install_id, now()],
            )
            .map_err(|e| Error::internal(e.to_string()))?;
            tx.execute("UPDATE record_schema SET version=?", [FILE_SCHEMA])
                .map_err(|e| Error::internal(e.to_string()))?;
            tx.commit().map_err(|e| Error::internal(e.to_string()))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| Error::internal(format!("record file permissions refused: {e}")))?;
        } else {
            // A foreign, downgraded or corrupt file refuses here; the
            // operator recovers explicitly (restore from backup, remove
            // after inspection) — the daemon never heals it in place.
            let version: i64 = conn
                .query_row("SELECT version FROM record_schema", [], |r| r.get(0))
                .map_err(|_| Error::rejected(CORRUPT))?;
            if version != FILE_SCHEMA {
                return Err(Error::rejected(
                    "record file schema is unsupported; restore the installation backup or remove the file after inspection",
                ));
            }
            let identity: String = conn
                .query_row("SELECT install_id FROM record_identity", [], |r| r.get(0))
                .map_err(|_| Error::rejected(CORRUPT))?;
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
        }
        Ok(Self {
            install_id: install_id.to_string(),
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
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

    fn history_in(conn: &Connection, context: &str, id: &str) -> rusqlite::Result<Vec<Value>> {
        let mut stmt = conn.prepare(
            "SELECT revision,body_digest,actor,at FROM app_record_revisions WHERE context_id=? AND record_id=? ORDER BY revision",
        )?;
        let rows = stmt.query_map(params![context, id], |r| {
            Ok(json!({"revision": r.get::<_, i64>(0)?, "digest": r.get::<_, String>(1)?, "actor": r.get::<_, String>(2)?, "at": r.get::<_, f64>(3)?}))
        })?;
        rows.collect()
    }

    fn show_in(&self, conn: &Connection, context: &str, id: &str) -> Result<Value> {
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
        Ok(
            json!({"id": id, "install_id": self.install_id, "context_id": context, "kind": kind, "revision": revision, "digest": digest, "profile": profile, "history": history}),
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
        let conn = self.conn();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| Error::internal(e.to_string()))?;
        if let Some(existing) = tx
            .query_row(
                "SELECT body_digest FROM app_records WHERE context_id=? AND id=?",
                params![context, record_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| Error::internal(e.to_string()))?
        {
            if existing != digest {
                return Err(Error::rejected("record ID already holds different content"));
            }
            let result = json!({"record": self.show_in(&tx, context, record_id)?});
            tx.commit().map_err(|e| Error::internal(e.to_string()))?;
            return Ok(result);
        }
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
        let result = json!({"record": self.show_in(&tx, context, record_id)?});
        tx.commit().map_err(|e| Error::internal(e.to_string()))?;
        Ok(result)
    }

    pub fn app_record_show(&self, context: &str, record_id: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(record_id, "record ID")?;
        let conn = self.conn();
        Ok(json!({"record": self.show_in(&conn, context, record_id)?}))
    }

    pub fn app_record_list(&self, context: &str) -> Result<Value> {
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        let ids = conn
            .prepare("SELECT id FROM app_records WHERE context_id=? ORDER BY id LIMIT 101")
            .map_err(|e| Error::internal(e.to_string()))?
            .query_map([context], |r| r.get::<_, String>(0))
            .map_err(|e| Error::internal(e.to_string()))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| Error::internal(e.to_string()))?;
        if ids.len() > RECORD_LIMIT as usize {
            return Err(Error::rejected(
                "record inventory exceeds its supported bound",
            ));
        }
        let records = ids
            .iter()
            .map(|id| self.show_in(&conn, context, id))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"records": records}))
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
        let conn = self.conn();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| Error::internal(e.to_string()))?;
        let current = self.show_in(&tx, context, record_id)?;
        if current["revision"].as_i64() != Some(expected) {
            return Err(Error::rejected("record revision is stale"));
        }
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
        let result = json!({"record": self.show_in(&tx, context, record_id)?});
        tx.commit().map_err(|e| Error::internal(e.to_string()))?;
        Ok(result)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentState {
    Granted,
    Denied,
    Unknown,
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
    pub tags: Vec<String>,
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
        if self.schema != 1
            || self.display_name.is_empty()
            || self.display_name.len() > 120
            || self.display_name.trim() != self.display_name
            || self.display_name.chars().any(char::is_control)
        {
            return Err(Error::rejected(
                "record profile exceeds its supported shape or bounds",
            ));
        }
        if let Some(email) = &self.email {
            // Bounded single-@ shape check only; deliverability is a
            // later slice's problem. No content is echoed on refusal.
            let mut parts = email.split('@');
            let valid = email.len() <= 254
                && !email
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
                && matches!((parts.next(), parts.next(), parts.next()), (Some(user), Some(domain), None) if !user.is_empty() && domain.contains('.') && !domain.is_empty());
            if !valid {
                return Err(Error::rejected(
                    "record profile exceeds its supported shape or bounds",
                ));
            }
        }
        if self.tags.len() > 16
            || self.tags.iter().any(|tag| {
                tag.is_empty()
                    || tag.len() > 40
                    || !tag
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
        {
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
        let guard = match self.write_conn() {
            Ok(guard) => guard,
            Err(error) => {
                eprintln!("record audit event skipped: {error}");
                return;
            }
        };
        if Self::event(
            &guard,
            Self::DAEMON_STREAM,
            if created {
                "app_record_created"
            } else {
                "app_record_updated"
            },
            json!({"record_id": record, "install_id": install, "context_id": context, "revision": revision, "digest": digest, "actor": "operator"}),
        )
        .is_err()
        {
            eprintln!("record audit event skipped: event write refused");
        }
    }
}
