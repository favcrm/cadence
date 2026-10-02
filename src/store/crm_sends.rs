//! PII-free send-level intent for CRM campaigns (CAD-786).
//!
//! Per-recipient rows — emails, names, suppression state — live only
//! in the installation's own record file (see
//! `store/app_records.rs`). Core keeps exactly two things:
//!
//! - `crm_sends`: one row per approved send, the durable intent a
//!   crash reconciliation sweep resumes. No recipient bytes ever
//!   land here — states and timestamps only.
//! - `crm_unsubscribe_index`: sha256 of each unsubscribe token →
//!   `(install_id, context_id)`, so a bearer-token redemption can
//!   find the one record file it writes into. Hash only: the token
//!   itself and every recipient identity stay out of core.

use super::*;
use rusqlite::{params, OptionalExtension};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS crm_sends(
 install_id TEXT NOT NULL, context_id TEXT NOT NULL,
 send_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('sending','completed','closed')),
 created REAL NOT NULL, updated REAL NOT NULL,
 PRIMARY KEY(install_id,context_id,send_id));
CREATE TABLE IF NOT EXISTS crm_unsubscribe_index(
 token_hash TEXT PRIMARY KEY, install_id TEXT NOT NULL, context_id TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS crm_settings(
 key TEXT PRIMARY KEY, value TEXT NOT NULL,
 by TEXT NOT NULL, updated REAL NOT NULL);
";

/// One core send row — the crash-reconciliation unit.
pub struct CrmSend {
    pub state: String,
}

impl Store {
    /// Open the durable intent for an approved send. One row per
    /// `(install, context, send)`; a second approve of the same send
    /// can never mint a parallel intent because the App-side CAS on
    /// the send row itself already refused — this insert is its
    /// core witness.
    pub fn crm_send_open(&self, install: &str, context: &str, send_id: &str) -> Result<()> {
        self.write_tx(|conn| {

                    conn.execute(
                        "INSERT INTO crm_sends(install_id,context_id,send_id,state,created,updated) VALUES(?,?,?, 'sending', ?, ?)",
                        params![install, context, send_id, now(), now()],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                    Ok(())
        })
    }

    /// Transition one send's durable state. `completed` and `closed`
    /// are terminal — the reconciliation sweep reads `sending` only.
    pub fn crm_send_transition(
        &self,
        install: &str,
        context: &str,
        send_id: &str,
        state: &str,
    ) -> Result<()> {
        debug_assert!(matches!(state, "completed" | "closed"));
        self.write_tx(|conn| {

                    conn.execute(
                        "UPDATE crm_sends SET state=?, updated=? WHERE install_id=? AND context_id=? AND send_id=?",
                        params![state, now(), install, context, send_id],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                    Ok(())
        })
    }

    /// Every send whose durable intent is still live — the boot-time
    /// sweep's worklist.
    pub fn crm_sends_sending(&self) -> Result<Vec<(String, String, String)>> {
        self.read_tx(|conn| {

                    let stmt_sql = "SELECT install_id,context_id,send_id FROM crm_sends WHERE state='sending' ORDER BY created";
                    let rows = conn.query_vec(stmt_sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|rows| rows.into_iter().map(Ok::<_, rusqlite::Error>))
                        .map_err(|e| Error::internal(e.to_string()))?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row.map_err(|e| Error::internal(e.to_string()))?);
                    }
                    Ok(out)
        })
    }

    /// Record one unsubscribe token's home. `token_hash` is the
    /// token's sha256 hex — never the token — so the index itself
    /// cannot redeem anything.
    pub fn crm_unsubscribe_index_add(
        &self,
        token_hash: &str,
        install: &str,
        context: &str,
    ) -> Result<()> {
        self.write_tx(|conn| {

                    conn.execute(
                        "INSERT OR IGNORE INTO crm_unsubscribe_index(token_hash,install_id,context_id) VALUES(?,?,?)",
                        params![token_hash, install, context],
                    )
                    .map_err(|e| Error::internal(e.to_string()))?;
                    Ok(())
        })
    }

    /// Where a token's suppression rows belong, if it is one of ours.
    /// Unknown hashes return `None` — the redeem path answers every
    /// token the same way regardless.
    pub fn crm_unsubscribe_index_lookup(
        &self,
        token_hash: &str,
    ) -> Result<Option<(String, String)>> {
        self.read_tx(|conn| {
            conn.query_opt(
                "SELECT install_id,context_id FROM crm_unsubscribe_index WHERE token_hash=?",
                params![token_hash],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(|e| Error::internal(e.to_string()))
        })
    }

    /// One persisted operator setting — key/value in `crm_settings`.
    /// `None` clears it.
    pub fn crm_setting_set(&self, key: &str, value: Option<&str>, by: &str) -> Result<()> {
        self.write_tx(|conn| {

                    match value {
                        Some(value) => conn
                            .execute(
                                "INSERT INTO crm_settings(key,value,by,updated) VALUES(?,?,?,?)
                                 ON CONFLICT(key) DO UPDATE SET value=excluded.value, by=excluded.by, updated=excluded.updated",
                                params![key, value, by, now()],
                            )
                            .map(|_| ()),
                        None => conn
                            .execute("DELETE FROM crm_settings WHERE key=?", params![key])
                            .map(|_| ()),
                    }
                    .map_err(|e| Error::internal(e.to_string()))
        })
    }

    /// The current value of one persisted operator setting.
    pub fn crm_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT value FROM crm_settings WHERE key=?",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| Error::internal(e.to_string()))
    }

    /// Best-effort audit for the unsubscribe origin — the origin is
    /// public configuration, never a secret.
    pub fn note_crm_send_origin(&self, origin: Option<&str>) {
        if let Err(e) = self.write_tx(|tx| {
            Self::event(
                &*tx,
                Self::DAEMON_STREAM,
                "crm_send_origin_set",
                json!({"unsubscribe_origin": origin}),
            )
        }) {
            eprintln!("store: best-effort CRM send origin audit failed: {e}");
        }
    }
}
