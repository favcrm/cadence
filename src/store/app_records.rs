//! Host-managed per-installation app records (CAD-753).
//!
//! One SQLite store keyed by immutable installation ID; a context ID
//! scopes rows inside the installation. App code, frames, chat and
//! workers never see a database path or raw SQL — only the typed
//! operator actions in `daemon::app_records_rpc` reach these methods,
//! and the daemon proves the operator connection plus the
//! installation/context binding first. A project link is not a field
//! here and confers nothing.
//!
//! The first record kind seeds customer profile/consent data for a
//! later CRM slice; the install/context scoping, CAS revision and
//! attributed history are reusable for other record kinds. Updates
//! require the expected revision; a stale or concurrent write is
//! refused without mutation.

use super::*;
use crate::store::app_runs::material_digest;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RECORD_BODY_BYTES: usize = 16 * 1024;
pub const RECORD_LIMIT: i64 = 100;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_records(
 id TEXT NOT NULL, install_id TEXT NOT NULL, context_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('customer')),
 revision INTEGER NOT NULL CHECK(revision>0),
 body TEXT NOT NULL, body_digest TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL,
 PRIMARY KEY(install_id, context_id, id));
CREATE INDEX IF NOT EXISTS app_records_install ON app_records(install_id, context_id);
CREATE TABLE IF NOT EXISTS app_record_revisions(
 install_id TEXT NOT NULL, context_id TEXT NOT NULL, record_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 body TEXT NOT NULL, body_digest TEXT NOT NULL,
 actor TEXT NOT NULL, at REAL NOT NULL,
 PRIMARY KEY(install_id, context_id, record_id, revision));
";

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
    fn require_record_context_in(
        conn: &Connection,
        install: &str,
        context: &str,
        write: bool,
    ) -> Result<()> {
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM app_contexts WHERE id=? AND install_id=?",
                params![context, install],
                |r| r.get(0),
            )
            .optional()?;
        match (state.as_deref(), write) {
            (Some("active"), _) => Ok(()),
            // Reads serve retained history after archival; writes stop.
            (Some(_), false) => Ok(()),
            _ => Err(Error::rejected(
                "record context is unavailable for this installation",
            )),
        }
    }

    fn app_record_history_in(
        conn: &Connection,
        install: &str,
        context: &str,
        id: &str,
    ) -> Result<Vec<Value>> {
        let mut stmt = conn.prepare(
            "SELECT revision,body_digest,actor,at FROM app_record_revisions WHERE install_id=? AND context_id=? AND record_id=? ORDER BY revision",
        )?;
        let rows = stmt
            .query_map(params![install, context, id], |r| {
                Ok(json!({"revision": r.get::<_, i64>(0)?, "digest": r.get::<_, String>(1)?, "actor": r.get::<_, String>(2)?, "at": r.get::<_, f64>(3)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn app_record_show_in(
        conn: &Connection,
        install: &str,
        context: &str,
        id: &str,
    ) -> Result<Value> {
        let (revision, kind, body, digest): (i64, String, String, String) = conn
            .query_row(
                "SELECT revision,kind,body,body_digest FROM app_records WHERE install_id=? AND context_id=? AND id=?",
                params![install, context, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("record is unavailable for this installation and context"))?;
        if kind != "customer" || revision < 1 || body.len() > RECORD_BODY_BYTES {
            return Err(Error::rejected("record integrity refused"));
        }
        let profile: CustomerProfile =
            serde_json::from_str(&body).map_err(|_| Error::rejected("record integrity refused"))?;
        if profile.digest(install, context)? != digest {
            return Err(Error::rejected("record integrity refused"));
        }
        Ok(
            json!({"id": id, "install_id": install, "context_id": context, "kind": kind, "revision": revision, "digest": digest, "profile": profile, "history": Self::app_record_history_in(conn, install, context, id)?}),
        )
    }

    pub fn app_record_create(
        &self,
        install: &str,
        context: &str,
        record_id: &str,
        profile: &CustomerProfile,
    ) -> Result<Value> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(record_id, "record ID")?;
        let digest = profile.digest(install, context)?;
        let body = serde_json::to_string(profile).map_err(|e| Error::internal(e.to_string()))?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        Self::require_record_context_in(&tx, install, context, true)?;
        if let Some(existing) = tx
            .query_row(
                "SELECT body_digest FROM app_records WHERE install_id=? AND context_id=? AND id=?",
                params![install, context, record_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            if existing != digest {
                return Err(Error::rejected("record ID already holds different content"));
            }
            let result =
                json!({"record": Self::app_record_show_in(&tx, install, context, record_id)?});
            tx.commit()?;
            return Ok(result);
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM app_records WHERE install_id=? AND context_id=?",
            params![install, context],
            |r| r.get(0),
        )?;
        if count >= RECORD_LIMIT {
            return Err(Error::rejected("context has reached its record limit"));
        }
        tx.execute(
            "INSERT INTO app_records(id,install_id,context_id,kind,revision,body,body_digest,created,updated) VALUES(?, ?, ?, 'customer', 1, ?, ?, ?, ?)",
            params![record_id, install, context, body, digest, now(), now()],
        )?;
        tx.execute(
            "INSERT INTO app_record_revisions(install_id,context_id,record_id,revision,body,body_digest,actor,at) VALUES(?, ?, ?, 1, ?, ?, 'operator', ?)",
            params![install, context, record_id, body, digest, now()],
        )?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            "app_record_created",
            json!({"record_id": record_id, "install_id": install, "context_id": context, "revision": 1, "digest": digest, "actor": "operator"}),
        )?;
        let result = json!({"record": Self::app_record_show_in(&tx, install, context, record_id)?});
        tx.commit()?;
        Ok(result)
    }

    pub fn app_record_show(&self, install: &str, context: &str, record_id: &str) -> Result<Value> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(record_id, "record ID")?;
        let conn = self.conn();
        Self::require_record_context_in(&conn, install, context, false)?;
        Ok(json!({"record": Self::app_record_show_in(&conn, install, context, record_id)?}))
    }

    pub fn app_record_list(&self, install: &str, context: &str) -> Result<Value> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(context, "context ID")?;
        let conn = self.conn();
        Self::require_record_context_in(&conn, install, context, false)?;
        let ids = conn
            .prepare("SELECT id FROM app_records WHERE install_id=? AND context_id=? ORDER BY id LIMIT 101")?
            .query_map(params![install, context], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.len() > RECORD_LIMIT as usize {
            return Err(Error::rejected(
                "record inventory exceeds its supported bound",
            ));
        }
        Ok(
            json!({"records": ids.iter().map(|id| Self::app_record_show_in(&conn, install, context, id)).collect::<Result<Vec<_>>>()?}),
        )
    }

    pub fn app_record_update(
        &self,
        install: &str,
        context: &str,
        record_id: &str,
        expected: i64,
        profile: &CustomerProfile,
    ) -> Result<Value> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(context, "context ID")?;
        crate::proto::identifier(record_id, "record ID")?;
        if expected < 1 {
            return Err(Error::rejected("expected record revision must be positive"));
        }
        let digest = profile.digest(install, context)?;
        let body = serde_json::to_string(profile).map_err(|e| Error::internal(e.to_string()))?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        Self::require_record_context_in(&tx, install, context, true)?;
        let current = Self::app_record_show_in(&tx, install, context, record_id)?;
        if current["revision"].as_i64() != Some(expected) {
            return Err(Error::rejected("record revision is stale"));
        }
        let revision = expected
            .checked_add(1)
            .ok_or_else(|| Error::rejected("record revision exhausted"))?;
        let changed = tx.execute(
            "UPDATE app_records SET revision=?, body=?, body_digest=?, updated=? WHERE install_id=? AND context_id=? AND id=? AND revision=?",
            params![revision, body, digest, now(), install, context, record_id, expected],
        )?;
        if changed != 1 {
            return Err(Error::rejected("record revision is stale"));
        }
        tx.execute(
            "INSERT INTO app_record_revisions(install_id,context_id,record_id,revision,body,body_digest,actor,at) VALUES(?, ?, ?, ?, ?, ?, 'operator', ?)",
            params![install, context, record_id, revision, body, digest, now()],
        )?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            "app_record_updated",
            json!({"record_id": record_id, "install_id": install, "context_id": context, "revision": revision, "digest": digest, "actor": "operator"}),
        )?;
        let result = json!({"record": Self::app_record_show_in(&tx, install, context, record_id)?});
        tx.commit()?;
        Ok(result)
    }
}
