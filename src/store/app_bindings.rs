//! Configured app bindings are immutable scoped receipts, never grants.
use super::*;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_bindings(
 id TEXT PRIMARY KEY, install_id TEXT NOT NULL, context_id TEXT,
 scope_key TEXT NOT NULL, slot TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 state TEXT NOT NULL CHECK(state IN ('configured','revoked')),
 config TEXT NOT NULL, digest TEXT NOT NULL, request_id TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL,
 UNIQUE(install_id,request_id));
CREATE UNIQUE INDEX IF NOT EXISTS app_binding_live_slot
 ON app_bindings(install_id,scope_key,slot) WHERE state='configured';
";
const CONFIG_BYTES: usize = 16 * 1024;
const LIST_MAX: i64 = 100;
const ACTIVE_BUNDLE_MAX: i64 = 100;
const LIFETIME_MAX: i64 = 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingProof {
    pub id: String,
    pub revision: i64,
    pub digest: String,
    pub config: Value,
}

fn scope_key(context: Option<&str>) -> String {
    context.map_or_else(|| "installation".into(), |id| format!("context:{id}"))
}

fn validate_config(install: &str, context: Option<&str>, config: &Value) -> Result<()> {
    crate::proto::identifier(install, "installation id")?;
    if let Some(id) = context {
        crate::proto::identifier(id, "context id")?;
    }
    if !config.is_object()
        || config["schema"] != json!(1)
        || config["install_id"].as_str() != Some(install)
        || match context {
            Some(id) => config["context"]["id"].as_str() != Some(id),
            None => !config.get("context").is_some_and(Value::is_null),
        }
    {
        return Err(Error::rejected(
            "binding configuration scope does not match its installation/context",
        ));
    }
    if serde_json::to_vec(config)?.len() > CONFIG_BYTES {
        return Err(Error::rejected("binding configuration exceeds 16 KiB"));
    }
    Ok(())
}

fn config_digest(install: &str, context: Option<&str>, slot: &str, config: &Value) -> String {
    app_runs::material_digest(&json!({"kind":"app-binding-v1", "install_id":install,
        "context_id":context,"slot":slot,"config":config}))
}

/// CAD-796: the binding fields covered by installation approval. A rebind
/// that changes any of these withdraws the installation's prior approval;
/// anything else (receipt formatting the daemon re-derives) never churns it.
fn binding_material_same(old: &Value, new: &Value) -> bool {
    const FIELDS: &[&str] = &[
        "install_id",
        "context",
        "bundle_digest",
        "workspace_id",
        "connection_id",
        "connection_kind",
        "connection_revision",
        "provider",
        "account",
        "registration_digest",
        "descriptor_revision",
        "mapping",
        "declaration",
    ];
    FIELDS.iter().all(|field| old[field] == new[field])
}

fn binding_in(conn: &Connection, install: &str, id: &str) -> Result<Value> {
    let row = conn.query_row(
        "SELECT context_id,slot,revision,state,config,digest FROM app_bindings WHERE install_id=? AND id=?",
        params![install,id], |r| Ok((r.get::<_,Option<String>>(0)?,r.get::<_,String>(1)?,
            r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)),
    ).optional()?.ok_or_else(|| Error::rejected("binding is not in this installation"))?;
    let config: Value = serde_json::from_str(&row.4)?;
    if config_digest(install, row.0.as_deref(), &row.1, &config) != row.5 {
        return Err(Error::rejected("binding configuration receipt is corrupt"));
    }
    Ok(
        json!({"id":id,"install_id":install,"context_id":row.0,"slot":row.1,
        "revision":row.2,"state":row.3,"config":config,"digest":row.5}),
    )
}

pub(crate) fn binding_current_in(
    conn: &Connection,
    install: &str,
    context: Option<&str>,
    slot: &str,
    proof: &BindingProof,
) -> Result<bool> {
    let row = binding_in(conn, install, &proof.id)?;
    let mut current = row["context_id"] == json!(context)
        && row["slot"] == slot
        && row["state"] == "configured"
        && row["revision"] == proof.revision
        && row["digest"] == proof.digest
        && row["config"] == proof.config;
    let workspace: String = conn.query_row(
        "SELECT workspace_id FROM connection_metadata WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    current &= proof.config["workspace_id"] == workspace;
    if let Some(context) = context {
        current &= conn.query_row("SELECT EXISTS(SELECT 1 FROM app_contexts WHERE install_id=? AND id=? AND revision=? AND config_digest=? AND state='active')",params![install,context,proof.config["context"]["revision"].as_i64(),proof.config["context"]["digest"].as_str()],|r|r.get::<_,bool>(0))?;
    } else {
        current &= proof.config["context"].is_null();
    }
    match proof.config["connection_kind"].as_str() {
        Some("builtin") => {
            let provider = proof.config["provider"]
                .as_str()
                .ok_or_else(|| Error::rejected("binding provider receipt is missing"))?;
            let account = proof.config["account"]
                .as_str()
                .ok_or_else(|| Error::rejected("binding account receipt is missing"))?;
            let expected = format!(
                "builtin-{}",
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!("{workspace}:{provider}:{account}").as_bytes()
                )
                .simple()
            );
            current &= proof.config["connection_id"] == expected
                && proof.config["connection_revision"].is_null();
        }
        Some("enrolled") => {
            let scopes = conn.query_row("SELECT scopes FROM platform_credentials WHERE connection_id=? AND credential_revision=? AND platform=? AND account=?",params![proof.config["connection_id"].as_str(),proof.config["connection_revision"].as_i64(),proof.config["provider"].as_str(),proof.config["account"].as_str()],|r|r.get::<_,String>(0)).optional()?;
            current &= if let Some(scopes) = scopes {
                let scopes: Vec<String> = serde_json::from_str(&scopes)?;
                let required: Vec<String> =
                    serde_json::from_value(proof.config["mapping"]["scopes"].clone())?;
                required.iter().all(|scope| scopes.contains(scope))
            } else {
                false
            };
        }
        _ => current = false,
    }
    Ok(current)
}

impl Store {
    /// CAD-796: withdraw one installation's capability approval after a
    /// material binding change. The row moves to `revoked` with a fresh
    /// epoch, mirroring an explicit operator revoke, so new runs refuse
    /// with "approval is absent or stale" until the operator re-approves
    /// the changed binding. The current digest's epoch rows move with it,
    /// like a re-approval supersede; other digests keep their exact
    /// historical authority.
    /// Answers whether an approval was withdrawn (absent stays absent).
    pub(super) fn app_binding_approval_withdraw_in(
        tx: &Connection,
        install: &str,
        reason: &str,
    ) -> Result<bool> {
        let current: Option<(String, i64)> = tx
            .query_row(
                "SELECT digest, epoch FROM app_install_capabilities WHERE install_id=? AND state='approved'",
                [install],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((digest, epoch)) = current else {
            return Ok(false);
        };
        let next = epoch + 1;
        tx.execute(
            "UPDATE app_install_capabilities SET state='revoked', epoch=?, created=? WHERE install_id=?",
            params![next, now(), install],
        )?;
        tx.execute(
            "UPDATE app_capability_epochs SET state='revoked' WHERE install_id=? AND digest=?",
            params![install, digest],
        )?;
        tx.execute(
            "INSERT INTO app_capability_epochs(install_id,epoch,digest,state,created) VALUES(?,?,?,?,?)",
            params![install, next, digest, "revoked", now()],
        )?;
        Self::event(
            tx,
            Self::DAEMON_STREAM,
            "app_install_capability_revoked",
            json!({"install_id": install, "digest": digest, "epoch": next,
                   "actor": "operator", "reason": reason}),
        )?;
        Ok(true)
    }

    /// CAD-796: drop every derived-grant row recorded for one
    /// installation. A scope survives only while some remaining
    /// derivation (any app, any install) still covers it, so a rebind
    /// never cuts another install's grant and never keeps this one's.
    pub(super) fn app_install_grants_drop_in(
        tx: &Connection,
        install_id: &str,
        by: &str,
    ) -> Result<()> {
        let apps: Vec<String> = tx
            .prepare("SELECT DISTINCT app FROM app_grants WHERE install_id=?")?
            .query_map([install_id], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for app in apps {
            let rows: Vec<(String, String, String, Vec<String>)> = {
                let mut stmt = tx.prepare(
                    "SELECT agent, platform, account, scopes FROM app_grants WHERE app=? AND install_id=?",
                )?;
                stmt.query_map(params![app, install_id], |r| {
                    let raw: String = r.get(3)?;
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default(),
                    ))
                })?
                .flatten()
                .collect()
            };
            if rows.is_empty() {
                continue;
            }
            tx.execute(
                "DELETE FROM app_grants WHERE app=? AND install_id=?",
                params![app, install_id],
            )?;
            for (agent, plat, account, derived) in &rows {
                let still: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT scopes FROM app_grants WHERE agent=? AND platform=? AND account=?",
                    )?;
                    stmt.query_map(params![agent, plat, account], |r| {
                        let raw: String = r.get(0)?;
                        Ok(serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default())
                    })?
                    .flatten()
                    .flatten()
                    .collect()
                };
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT scopes FROM platform_grants WHERE agent=? AND platform=? AND account=?",
                        params![agent, plat, account],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(raw) = existing else {
                    continue;
                };
                let held: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
                let kept: Vec<String> = held
                    .iter()
                    .filter(|s| !derived.contains(s) || still.contains(s))
                    .cloned()
                    .collect();
                if kept == held {
                    continue;
                }
                if kept.is_empty() {
                    tx.execute(
                        "DELETE FROM platform_grants WHERE agent=? AND platform=? AND account=?",
                        params![agent, plat, account],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE platform_grants SET scopes=? WHERE agent=? AND platform=? AND account=?",
                        params![serde_json::to_string(&kept)?, agent, plat, account],
                    )?;
                }
            }
            Self::event(
                tx,
                platform::PLATFORM_STREAM,
                platform::APP_GRANTS_REVOKED_EVENT,
                json!({"install_id": install_id, "app": app,
                       "reason": "connection_binding_changed", "by": by}),
            )?;
        }
        Ok(())
    }

    /// CAD-796: close every configured binding on one credential and
    /// withdraw each affected installation's approval. Called inside the
    /// credential revoke/rotate transaction, so the record change and
    /// the approval withdrawal land together.
    pub(super) fn app_binding_approvals_withdraw_for_credential_in(
        tx: &Connection,
        platform_name: &str,
        account: &str,
    ) -> Result<()> {
        let rows: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT install_id, id FROM app_bindings WHERE state='configured' AND json_extract(config,'$.provider')=? AND json_extract(config,'$.account')=?",
            )?;
            stmt.query_map(params![platform_name, account], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .flatten()
            .collect()
        };
        if rows.is_empty() {
            return Ok(());
        }
        for (install, binding) in &rows {
            Self::app_effect_invalidate_in(tx, install, None, Some(binding), None)?;
        }
        let mut installs: Vec<String> = rows.into_iter().map(|(install, _)| install).collect();
        installs.sort();
        installs.dedup();
        for install in installs {
            Self::app_binding_approval_withdraw_in(tx, &install, "connection_credential_changed")?;
            Self::app_install_grants_drop_in(tx, &install, "operator")?;
        }
        Ok(())
    }
}

impl Store {
    pub fn app_binding_show(&self, install: &str, id: &str) -> Result<Value> {
        Ok(json!({"binding":binding_in(&self.conn(), install, id)?}))
    }

    /// An omitted context filter lists both installation and context bindings.
    /// The current catalog digest is prioritized so retained old versions do
    /// not hide a newly configured binding inside the bounded response.
    pub fn app_binding_list(&self, install: &str, context: Option<&str>) -> Result<Value> {
        self.app_binding_list_preferred(install, context, None)
    }
    pub fn app_binding_list_preferred(
        &self,
        install: &str,
        context: Option<&str>,
        preferred_digest: Option<&str>,
    ) -> Result<Value> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id FROM app_bindings WHERE install_id=? AND (? IS NULL OR context_id=?)
            ORDER BY CASE WHEN ? IS NOT NULL AND json_extract(config,'$.bundle_digest')=? THEN 0 ELSE 1 END,
                     CASE WHEN state='configured' THEN 0 ELSE 1 END, created DESC,id DESC LIMIT 101")?;
        let mut ids = stmt
            .query_map(
                params![
                    install,
                    context,
                    context,
                    preferred_digest,
                    preferred_digest
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let truncated = ids.len() > LIST_MAX as usize;
        ids.truncate(LIST_MAX as usize);
        let bindings = ids
            .iter()
            .map(|id| binding_in(&conn, install, id))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"bindings":bindings,"truncated":truncated}))
    }

    /// Upgrade compatibility must inspect every configured binding, including
    /// rows intentionally hidden by the bounded operator inventory.
    pub fn app_binding_upgrade_configured(&self, install: &str) -> Result<Vec<Value>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM app_bindings WHERE install_id=? AND state='configured' ORDER BY id",
        )?;
        let ids = stmt
            .query_map([install], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| binding_in(&conn, install, id))
            .collect()
    }

    /// Called under the PM upgrade lock before any journal write. Every
    /// declared slot must be usable in at least one scope after the upgrade.
    pub fn app_binding_upgrade_capacity(
        &self,
        install: &str,
        bundle_digest: &str,
        slots: &[String],
    ) -> Result<()> {
        if slots.is_empty() {
            return Ok(());
        }
        let conn = self.conn();
        let total: i64 = conn.query_row(
            "SELECT count(*) FROM app_bindings WHERE install_id=?",
            [install],
            |r| r.get(0),
        )?;
        let active: i64 = conn.query_row(
            "SELECT count(*) FROM app_bindings WHERE install_id=? AND state='configured'
             AND json_extract(config,'$.bundle_digest')=?",
            params![install, bundle_digest],
            |r| r.get(0),
        )?;
        let mut missing = 0;
        for slot in slots {
            let present: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM app_bindings WHERE install_id=? AND state='configured'
                 AND json_extract(config,'$.bundle_digest')=? AND slot=?)",
                params![install, bundle_digest, slot],
                |r| r.get(0),
            )?;
            if !present {
                missing += 1;
            }
        }
        if total + missing > LIFETIME_MAX || active + missing > ACTIVE_BUNDLE_MAX {
            return Err(Error::rejected(
                "binding capacity cannot reserve the new package's declared slots; retain evidence or choose another installation",
            ));
        }
        Ok(())
    }

    pub fn app_binding_create(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        config: &Value,
        request: &str,
    ) -> Result<Value> {
        validate_config(install, context, config)?;
        crate::proto::identifier(slot, "publication slot")?;
        crate::proto::identifier(request, "binding request id")?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let existing = tx
            .query_row(
                "SELECT id FROM app_bindings WHERE install_id=? AND request_id=?",
                params![install, request],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let row = binding_in(&tx, install, &id)?;
            if row["context_id"] != json!(context)
                || row["slot"] != slot
                || row["config"] != *config
            {
                return Err(Error::rejected(
                    "binding request id is already used for different material",
                ));
            }
            tx.commit()?;
            return Ok(json!({"binding":row}));
        }
        let total: i64 = tx.query_row(
            "SELECT count(*) FROM app_bindings WHERE install_id=?",
            [install],
            |r| r.get(0),
        )?;
        if total >= LIFETIME_MAX {
            return Err(Error::rejected(
                "installation has reached its lifetime binding capacity",
            ));
        }
        let active: i64 = tx.query_row(
            "SELECT count(*) FROM app_bindings WHERE install_id=? AND state='configured'
             AND json_extract(config,'$.bundle_digest')=?",
            params![install, config["bundle_digest"].as_str()],
            |r| r.get(0),
        )?;
        if active >= ACTIVE_BUNDLE_MAX {
            return Err(Error::rejected(
                "this package version has reached its configured binding capacity",
            ));
        }
        let occupied: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM app_bindings WHERE install_id=? AND scope_key=? AND slot=? AND coalesce(json_extract(config,'$.bundle_digest'),'')=coalesce(?,'') AND state='configured')",params![install,scope_key(context),slot,config["bundle_digest"].as_str()],|r| r.get(0))?;
        if occupied {
            return Err(Error::rejected(
                "this scope already has a configured binding for this slot",
            ));
        }
        let id = format!("binding-{}", uuid::Uuid::new_v4().simple());
        let digest = config_digest(install, context, slot, config);
        tx.execute(
            "INSERT INTO app_bindings VALUES(?,?,?,?,?,1,'configured',?,?,?, ?,?)",
            params![
                id,
                install,
                context,
                scope_key(context),
                slot,
                config.to_string(),
                digest,
                request,
                now(),
                now()
            ],
        )?;
        Self::event(
            &tx,
            "app_bindings",
            "app_binding_configured",
            json!({"install_id":install,"binding_id":id,"revision":1,"digest":digest}),
        )?;
        let row = binding_in(&tx, install, &id)?;
        tx.commit()?;
        Ok(json!({"binding":row}))
    }

    pub fn app_binding_update(
        &self,
        install: &str,
        id: &str,
        expected: i64,
        config: &Value,
    ) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let row = binding_in(&tx, install, id)?;
        let revision = row["revision"]
            .as_i64()
            .ok_or_else(|| Error::internal("invalid binding revision"))?;
        if revision != expected {
            return Err(Error::conflict(revision, "binding revision changed"));
        }
        if row["state"] != "configured" {
            return Err(Error::rejected("revoked binding cannot be updated"));
        }
        let context = row["context_id"].as_str();
        validate_config(install, context, config)?;
        if row["config"]["bundle_digest"] != config["bundle_digest"] {
            return Err(Error::rejected(
                "a new bundle needs a new version-pinned binding; update cannot rewrite an old version",
            ));
        }
        if row["config"] == *config {
            // Identical re-save: keep the revision, keep the approval.
            tx.commit()?;
            return Ok(json!({"binding": row}));
        }
        let material_changed = !binding_material_same(&row["config"], config);
        let slot = row["slot"]
            .as_str()
            .ok_or_else(|| Error::internal("invalid binding slot"))?;
        let digest = config_digest(install, context, slot, config);
        let updated = tx.execute("UPDATE app_bindings SET config=?,digest=?,revision=revision+1,updated=? WHERE install_id=? AND id=? AND revision=? AND state='configured'",params![config.to_string(),digest,now(),install,id,expected])?;
        if updated != 1 {
            return Err(Error::rejected("binding update lost its revision claim"));
        }
        Self::app_effect_invalidate_in(&tx, install, None, Some(id), None)?;
        if material_changed {
            Self::app_binding_approval_withdraw_in(&tx, install, "connection_binding_changed")?;
            Self::app_install_grants_drop_in(&tx, install, "operator")?;
        }
        Self::event(
            &tx,
            "app_bindings",
            "app_binding_configured",
            json!({"install_id":install,"binding_id":id,"revision":revision+1,"digest":digest}),
        )?;
        let next = binding_in(&tx, install, id)?;
        tx.commit()?;
        Ok(json!({"binding":next}))
    }

    pub fn app_binding_revoke(&self, install: &str, id: &str, expected: i64) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let row = binding_in(&tx, install, id)?;
        let revision = row["revision"]
            .as_i64()
            .ok_or_else(|| Error::internal("invalid binding revision"))?;
        if revision != expected {
            return Err(Error::conflict(revision, "binding revision changed"));
        }
        if row["state"] != "configured" {
            return Err(Error::rejected("binding is already revoked"));
        }
        let updated = tx.execute("UPDATE app_bindings SET state='revoked',revision=revision+1,updated=? WHERE install_id=? AND id=? AND revision=? AND state='configured'",params![now(),install,id,expected])?;
        if updated != 1 {
            return Err(Error::rejected("binding revoke lost its revision claim"));
        }
        Self::app_effect_invalidate_in(&tx, install, None, Some(id), None)?;
        Self::app_binding_approval_withdraw_in(&tx, install, "connection_binding_changed")?;
        Self::app_install_grants_drop_in(&tx, install, "operator")?;
        Self::event(
            &tx,
            "app_bindings",
            "app_binding_revoked",
            json!({"install_id":install,"binding_id":id,"revision":revision+1}),
        )?;
        let next = binding_in(&tx, install, id)?;
        tx.commit()?;
        Ok(json!({"binding":next}))
    }

    pub fn app_binding_for_slot(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        bundle_digest: &str,
    ) -> Result<Option<BindingProof>> {
        let conn = self.conn();
        let id = conn.query_row("SELECT id FROM app_bindings WHERE install_id=? AND scope_key=? AND slot=? AND json_extract(config,'$.bundle_digest')=? AND state='configured'",params![install,scope_key(context),slot,bundle_digest],|r| r.get::<_,String>(0)).optional()?;
        id.map(|id| {
            let row = binding_in(&conn, install, &id)?;
            Ok(BindingProof {
                id,
                revision: row["revision"]
                    .as_i64()
                    .ok_or_else(|| Error::internal("invalid binding revision"))?,
                digest: row["digest"]
                    .as_str()
                    .ok_or_else(|| Error::internal("invalid binding digest"))?
                    .into(),
                config: row["config"].clone(),
            })
        })
        .transpose()
    }
}
