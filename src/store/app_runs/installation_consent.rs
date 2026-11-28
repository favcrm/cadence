//! Installation consent, capability epochs, and upgrade admission.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::human_receipt;
use crate::error::{Error, Result};
use crate::store::{now, Store, StoreConn};

impl Store {
    pub fn app_capability_decide(&self, id: &str, digest: &str, approve: bool) -> Result<Value> {
        self.app_capability_decide_as(id, digest, approve, None)
    }
    /// CAD-1374: the install approval or revocation, stamped with the verified
    /// human when the owner's board made it. Same transaction as before.
    pub fn app_capability_decide_as(
        &self,
        id: &str,
        digest: &str,
        approve: bool,
        human: Option<&Value>,
    ) -> Result<Value> {
        let operation = uuid::Uuid::new_v4().to_string();
        let (action, outcome) = if approve {
            ("app_install_capability_approved", "approved")
        } else {
            ("app_install_capability_revoked", "revoked")
        };
        let audit = human_receipt(
            json!({"digest":digest}),
            human,
            &operation,
            action,
            &format!("install:{id}"),
            outcome,
        );
        self.app_capability_decide_audited(id, digest, approve, audit)
    }
    /// CAD-1119: installing or updating an app is the operator's consent.
    /// The operator install/upgrade path records the approval of exactly
    /// that installed digest, auditing who installed it, how, and the
    /// capabilities it declares. An approval already in force for this
    /// digest is left untouched: re-deciding the same digest would
    /// invalidate effects staged under it.
    pub fn app_install_consent(
        &self,
        id: &str,
        digest: &str,
        via: &str,
        capabilities: &Value,
    ) -> Result<Option<Value>> {
        if self.app_capability_status(id, digest)?["state"] == "approved" {
            return Ok(None);
        }
        self.app_capability_decide_audited(
            id,
            digest,
            true,
            json!({"via": via, "capabilities": capabilities}),
        )
        .map(Some)
    }
    /// CAD-1129 H5: revoke this install's consent at its current
    /// digest — the soft-remove path. `app_capability_decide_audited`
    /// already writes the `revoked` epoch; this is the operator-facing
    /// wrapper that never leaves an `approved` slot behind.
    pub fn app_install_revoke(&self, id: &str, digest: &str, via: &str) -> Result<Value> {
        self.app_capability_decide_audited(id, digest, false, json!({"via": via}))
    }
    fn app_capability_decide_audited(
        &self,
        id: &str,
        digest: &str,
        approve: bool,
        audit: Value,
    ) -> Result<Value> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
        let previous: Option<(String, String)> = tx
            .query_opt(
                "SELECT digest,state FROM app_install_capabilities WHERE install_id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
        tx.execute("INSERT INTO app_install_capabilities VALUES(?,1,?,?,?) ON CONFLICT(install_id) DO UPDATE SET epoch=epoch+1,digest=excluded.digest,state=excluded.state,created=excluded.created",params![id,digest,if approve{"approved"}else{"revoked"},now()])?;
        let epoch: i64 = tx.query_row(
            "SELECT epoch FROM app_install_capabilities WHERE install_id=?",
            [id],
            |r| r.get(0),
        )?;
        if approve {
            // Reapproving the same bundle supersedes its older epochs. A new
            // bundle leaves completed old-version work authorized by its
            // exact historical epoch and retained bundle bytes.
            tx.execute(
                "UPDATE app_capability_epochs SET state='revoked' WHERE install_id=? AND digest=?",
                params![id, digest],
            )?;
            if previous.as_ref().is_some_and(|(old, _)| old == digest) {
                Self::app_effect_invalidate_in(&tx, id, None, None, Some(digest))?;
            }
        } else {
            // An explicit installation revoke removes authority from every
            // version, including completed historical work.
            tx.execute(
                "UPDATE app_capability_epochs SET state='revoked' WHERE install_id=?",
                [id],
            )?;
            Self::app_effect_invalidate_in(&tx, id, None, None, None)?;
        }
        tx.execute("INSERT INTO app_capability_epochs(install_id,epoch,digest,state,created) VALUES(?,?,?,?,?)",
            params![id,epoch,digest,if approve{"approved"}else{"revoked"},now()])?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            if approve {
                "app_install_capability_approved"
            } else {
                "app_install_capability_revoked"
            },
            {
                let mut event =
                    json!({"install_id":id,"digest":digest,"epoch":epoch,"actor":"operator"});
                if let (Some(event), Some(audit)) = (event.as_object_mut(), audit.as_object()) {
                    for (key, value) in audit {
                        event.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
                event
            },
        )?;
        Ok(
            json!({"install_id":id,"epoch":epoch,"digest":digest,"approved":approve,"capabilities":["local.text.produce","local.text.review"],"outward_release":false}),
        )
        })
    }

    /// Called while holding the PM lock: admission cannot race another run
    /// creation. Terminal history stays in SQLite; no active turn is retired.
    pub fn app_install_upgrade_ready(&self, install: &str) -> Result<()> {
        let conn = self.conn();
        let unresolved_effects: i64 = conn.query_row(
            "SELECT count(*) FROM app_effect_authorizations a JOIN platform_effects e ON e.effect_id=a.effect_id WHERE a.install_id=? AND e.state IN ('waiting','decided','executing','reconcile')",
            [install],
            |row| row.get(0),
        )?;
        if unresolved_effects != 0 {
            return Err(Error::rejected(
                "installation has an unresolved app effect; decide or reconcile it before upgrade",
            ));
        }
        let active: i64 = conn.query_row(
            "SELECT count(*) FROM app_runs WHERE install_id=? AND state IN ('awaiting_approval','approved','running')",
            [install],
            |row| row.get(0),
        )?;
        if active != 0 {
            return Err(Error::rejected(
                "installation has a nonterminal run; finish or explicitly cancel it before upgrade",
            ));
        }
        let grants: i64 = conn.query_row(
            "SELECT count(*) FROM app_grants WHERE install_id=?",
            [install],
            |row| row.get(0),
        )?;
        if grants != 0 {
            return Err(Error::rejected(
                "installation has derived grants; revoke them before upgrade",
            ));
        }
        Ok(())
    }

    pub fn app_capability_status(&self, id: &str, digest: &str) -> Result<Value> {
        Ok(self
            .conn()
            .query_row(
                "SELECT epoch,state FROM app_install_capabilities WHERE install_id=? AND digest=?",
                params![id, digest],
                |r| Ok(json!({"epoch":r.get::<_,i64>(0)?,"state":r.get::<_,String>(1)?})),
            )
            .optional()?
            .unwrap_or(json!({"state":"unapproved"})))
    }

    /// The install's one consent row, whichever digest it currently names.
    /// An automatic update compares this with the digest it observed before
    /// the package fetch. No row means nobody has decided the install.
    pub fn app_install_consent_current(&self, id: &str) -> Result<Value> {
        Ok(self
            .conn()
            .query_row(
                "SELECT digest,state FROM app_install_capabilities WHERE install_id=?",
                [id],
                |r| Ok(json!({"digest": r.get::<_, String>(0)?, "state": r.get::<_, String>(1)?})),
            )
            .optional()?
            .unwrap_or(json!({"state": "unapproved"})))
    }
}
