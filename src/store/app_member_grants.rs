//! CAD-2215: explicit owner/operator grants that let one verified member read
//! one exact app conversation of the master.
//!
//! This is **not** `app_grants` (see `store::platform`): that table derives
//! credential scopes for an AGENT alias. A row here names a *person* by the
//! platform's exact `(org, sub)` pair, one live installation, and ONE exact
//! conversation. There is no installation-wide, context-wide, empty or
//! wildcard selector.
//!
//! `context_id` is not a selector: it is the conversation's exact context
//! captured when the grant was written and re-proved as an immutable fact at
//! read time. A conversation with no context must be the installation's
//! canonical General conversation, and it is represented by its exact
//! conversation id with a NULL context fact.
//!
//! `install_digest` pins the bundle the operator reviewed. A changed digest
//! makes the row inert until the operator re-grants it. Revoking retains the
//! row as a tombstone (`state='revoked'`) so the operator can list and audit
//! it after the member and their session are gone.
//!
//! A row is never authority by itself: the daemon re-proves the live reader
//! session, the live installation, its approved capability digest, the exact
//! conversation binding and every project/private-job denial before admitting
//! anything.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::{now, Store, StoreConn};
use crate::error::{Error, Result};

/// The v65 schema objects. `IF NOT EXISTS` so a half-applied migration
/// converges on reopen.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_member_grants(
 org TEXT NOT NULL,
 sub TEXT NOT NULL,
 install_id TEXT NOT NULL,
 conversation_id TEXT NOT NULL,
 context_id TEXT,
 install_digest TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('active','revoked')),
 granted_at REAL NOT NULL,
 granted_by TEXT NOT NULL,
 revoked_at REAL,
 revoked_by TEXT,
 PRIMARY KEY(org, sub, install_id, conversation_id));
CREATE INDEX IF NOT EXISTS app_member_grants_member
 ON app_member_grants(org, sub, state);
CREATE INDEX IF NOT EXISTS app_member_grants_install
 ON app_member_grants(install_id, org, state);
";

#[derive(Clone, Debug, PartialEq)]
pub struct MemberGrant {
    pub org: String,
    pub sub: String,
    pub install_id: String,
    pub conversation_id: String,
    /// The conversation's exact context at grant time; `None` is the
    /// installation's canonical contextless General conversation.
    pub context_id: Option<String>,
    pub install_digest: String,
    pub state: String,
    pub granted_at: f64,
    pub granted_by: String,
    pub revoked_at: Option<f64>,
    pub revoked_by: Option<String>,
    /// Changes on each explicit grant, including after a revoke.
    pub authorization_epoch: String,
}

impl MemberGrant {
    pub fn active(&self) -> bool {
        self.state == "active"
    }

    pub(crate) fn has_valid_authorization_epoch(&self) -> bool {
        self.authorization_epoch.len() == 32
            && self
                .authorization_epoch
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    /// The operator-facing row. `member_sub` is the exact grant key, and
    /// `conversation_id`/`context_id` are the exact binding.
    pub fn to_json(&self) -> Value {
        json!({
            "member_sub": self.sub,
            "install_id": self.install_id,
            "conversation_id": self.conversation_id,
            "context_id": self.context_id,
            "install_digest": self.install_digest,
            "state": self.state,
            "granted_at": self.granted_at,
            "granted_by": self.granted_by,
            "revoked_at": self.revoked_at,
            "revoked_by": self.revoked_by,
        })
    }
}

fn row_grant(row: &rusqlite::Row) -> rusqlite::Result<MemberGrant> {
    Ok(MemberGrant {
        org: row.get("org")?,
        sub: row.get("sub")?,
        install_id: row.get("install_id")?,
        conversation_id: row.get("conversation_id")?,
        context_id: row.get("context_id")?,
        install_digest: row.get("install_digest")?,
        state: row.get("state")?,
        granted_at: row.get("granted_at")?,
        granted_by: row.get("granted_by")?,
        revoked_at: row.get("revoked_at")?,
        revoked_by: row.get("revoked_by")?,
        authorization_epoch: row.get("authorization_epoch")?,
    })
}

impl Store {
    /// Every ACTIVE grant held by this exact `(org, sub)`. This is the read
    /// admission source; a tombstone is never returned here.
    pub fn app_member_grants_active(&self, org: &str, sub: &str) -> Result<Vec<MemberGrant>> {
        self.read_tx(|conn| Self::app_member_grants_active_in(&conn, org, sub))
    }

    fn app_member_grants_active_in(
        conn: &impl StoreConn,
        org: &str,
        sub: &str,
    ) -> Result<Vec<MemberGrant>> {
        Ok(conn.query_vec(
            "SELECT * FROM app_member_grants
             WHERE org=? AND sub=? AND state='active'
             ORDER BY install_id, conversation_id",
            params![org, sub],
            row_grant,
        )?)
    }

    /// Active and revoked rows for one installation, active first and newest
    /// first. Operator recovery reads this directly; it never consults a
    /// member session or the platform roster.
    pub fn app_member_grants_list(&self, org: &str, install_id: &str) -> Result<Vec<MemberGrant>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT * FROM app_member_grants WHERE org=? AND install_id=?
                 ORDER BY (state='active') DESC, granted_at DESC, sub, conversation_id",
                params![org, install_id],
                row_grant,
            )?)
        })
    }

    fn app_member_grant_in(
        conn: &impl StoreConn,
        org: &str,
        sub: &str,
        install_id: &str,
        conversation_id: &str,
    ) -> Result<Option<MemberGrant>> {
        Ok(conn
            .query_row(
                "SELECT * FROM app_member_grants
                 WHERE org=? AND sub=? AND install_id=? AND conversation_id=?",
                params![org, sub, install_id, conversation_id],
                row_grant,
            )
            .optional()?)
    }

    /// Create or re-activate one exact-conversation grant. Idempotent on the
    /// primary key: a revoked tombstone is flipped back to active with the
    /// newly reviewed digest and the conversation's current exact context
    /// fact.
    #[allow(clippy::too_many_arguments)]
    pub fn app_member_grant_set(
        &self,
        org: &str,
        sub: &str,
        install_id: &str,
        conversation_id: &str,
        context_id: Option<&str>,
        install_digest: &str,
        by: &str,
    ) -> Result<MemberGrant> {
        if org.is_empty() || sub.is_empty() || install_id.is_empty() || conversation_id.is_empty() {
            return Err(Error::rejected(
                "an app access grant needs the company, member subject, installation and conversation",
            ));
        }
        self.write_tx(|conn| {
            let at = now();
            let authorization_epoch = uuid::Uuid::new_v4().simple().to_string();
            conn.execute(
                "INSERT INTO app_member_grants(
                     org, sub, install_id, conversation_id, context_id,
                     install_digest, state, granted_at, granted_by,
                     revoked_at, revoked_by, authorization_epoch)
                 VALUES(?,?,?,?,?,?,'active',?,?,NULL,NULL,?)
                 ON CONFLICT(org, sub, install_id, conversation_id) DO UPDATE SET
                     context_id=excluded.context_id,
                     install_digest=excluded.install_digest,
                     state='active',
                     granted_at=excluded.granted_at,
                     granted_by=excluded.granted_by,
                     revoked_at=NULL,
                     revoked_by=NULL,
                     authorization_epoch=excluded.authorization_epoch",
                params![
                    org,
                    sub,
                    install_id,
                    conversation_id,
                    context_id,
                    install_digest,
                    at,
                    by,
                    authorization_epoch
                ],
            )?;
            Self::app_member_grant_in(&conn, org, sub, install_id, conversation_id)?
                .ok_or_else(|| Error::internal("app access grant did not persist"))
        })
    }

    /// Revoke one exact-conversation grant. Idempotent: a missing row is
    /// `Ok(None)`, and an already-revoked tombstone keeps its original
    /// revocation time.
    pub fn app_member_grant_revoke(
        &self,
        org: &str,
        sub: &str,
        install_id: &str,
        conversation_id: &str,
        by: &str,
    ) -> Result<Option<MemberGrant>> {
        self.write_tx(|conn| {
            let Some(existing) =
                Self::app_member_grant_in(&conn, org, sub, install_id, conversation_id)?
            else {
                return Ok(None);
            };
            if existing.active() {
                conn.execute(
                    "UPDATE app_member_grants
                     SET state='revoked', revoked_at=?, revoked_by=?
                     WHERE org=? AND sub=? AND install_id=? AND conversation_id=?",
                    params![now(), by, org, sub, install_id, conversation_id],
                )?;
            }
            Self::app_member_grant_in(&conn, org, sub, install_id, conversation_id)
        })
    }

    /// Revoke every exact-conversation grant this member holds for one
    /// installation. Recovery must work when the member has no live session
    /// and no assistant binding; every row stays an exact tombstone.
    pub fn app_member_grants_revoke_member_install(
        &self,
        org: &str,
        sub: &str,
        install_id: &str,
        by: &str,
    ) -> Result<Vec<MemberGrant>> {
        self.write_tx(|conn| {
            let at = now();
            conn.execute(
                "UPDATE app_member_grants
                 SET state='revoked', revoked_at=?, revoked_by=?
                 WHERE org=? AND sub=? AND install_id=? AND state='active'",
                params![at, by, org, sub, install_id],
            )?;
            Ok(conn.query_vec(
                "SELECT * FROM app_member_grants
                 WHERE org=? AND sub=? AND install_id=?
                 ORDER BY conversation_id",
                params![org, sub, install_id],
                row_grant,
            )?)
        })
    }
}
