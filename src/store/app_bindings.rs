//! Configured app bindings are immutable scoped receipts, never grants.
use super::StoreConn;
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
    if config["provider"] == crate::platform::account_media::PLATFORM {
        let source: crate::platform::account_media::AccountSource =
            serde_json::from_value(config["account_source"].clone()).map_err(|_| {
                Error::rejected("account image binding requires an exact source receipt")
            })?;
        if config["account"] != source.handle() || config["connection_kind"] != "builtin" {
            return Err(Error::rejected(
                "account image binding source does not match its connection",
            ));
        }
    } else if config.get("account_source").is_some() {
        return Err(Error::rejected(
            "only account images accept an account source receipt",
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

/// CAD-796: the binding fields a derived grant rests on. A rebind that
/// changes any of these drops the installation's derived grants; anything
/// else (receipt formatting the daemon re-derives) never churns them. Since
/// CAD-1119 the rebind itself is the operator's consent for the slot, so it
/// no longer withdraws the installation's approval.
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
        "account_source",
        "registration_digest",
        "descriptor_revision",
        "mapping",
        "declaration",
    ];
    FIELDS.iter().all(|field| old[field] == new[field])
}

/// CAD-1119: how a configured binding's stored receipt differs from the
/// receipt the daemon re-derives for the same connection now.
#[derive(Debug, Clone, PartialEq)]
pub enum BindingDrift {
    /// Byte-identical: the binding is current.
    Same,
    /// Only provider bookkeeping moved (a descriptor or deployment pin
    /// revision, the registration receipt, or the provider's tool name for
    /// the same reviewed action). The binding migrates without the operator.
    Compatible(Vec<Value>),
    /// Anything else changed — capability, version, action, resource kind,
    /// effect, semantics, scopes, contracts or the connection itself. The
    /// slot stops until the operator confirms by binding it again.
    NeedsConfirm(Vec<Value>),
}

/// Top-level receipt fields a migration may carry forward. Allowlist: a
/// field not named here (including any future one) needs the operator.
const MIGRATABLE_RECEIPT: &[&str] = &[
    "registration_digest",
    "descriptor_revision",
    "sink_registration",
    "reviewed_pin",
    "reported_pin",
];
/// The one mapping field that may move: the provider's own name for the
/// same reviewed action. Every other mapping field is the slot contract.
const MIGRATABLE_MAPPING: &[&str] = &["tool"];

fn drift_keys(old: &Value, new: &Value) -> Vec<String> {
    let mut keys: Vec<String> = old
        .as_object()
        .into_iter()
        .chain(new.as_object())
        .flat_map(|map| map.keys().cloned())
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Classify a stored binding receipt against a freshly derived one.
pub fn binding_drift(old: &Value, new: &Value) -> BindingDrift {
    if old == new {
        return BindingDrift::Same;
    }
    // A receipt is a JSON object. Anything else cannot be classified, so it
    // never migrates (no keys would mean "nothing widened": fail closed).
    if !old.is_object() || !new.is_object() {
        return BindingDrift::NeedsConfirm(vec![
            json!({"field": "receipt", "from": old, "to": new}),
        ]);
    }
    let (mut compatible, mut widened) = (Vec::new(), Vec::new());
    let mut note = |field: String, from: &Value, to: &Value, allowed: bool| {
        let change = json!({"field": field, "from": from, "to": to});
        if allowed {
            compatible.push(change);
        } else {
            widened.push(change);
        }
    };
    for key in drift_keys(old, new) {
        let (from, to) = (&old[&key], &new[&key]);
        // `null` and absent index the same; presence is part of the receipt.
        if from == to && old.get(&key).is_some() == new.get(&key).is_some() {
            continue;
        }
        if key == "mapping" && from.is_object() && to.is_object() {
            for sub in drift_keys(from, to) {
                let both = from.get(&sub).is_some() && to.get(&sub).is_some();
                if from[&sub] != to[&sub] || (!both && from.get(&sub) != to.get(&sub)) {
                    // An added or removed field is never bookkeeping.
                    let allowed = both && MIGRATABLE_MAPPING.contains(&sub.as_str());
                    note(format!("mapping.{sub}"), &from[&sub], &to[&sub], allowed);
                }
            }
        } else {
            let both = old.get(&key).is_some() && new.get(&key).is_some();
            note(
                key.clone(),
                from,
                to,
                both && MIGRATABLE_RECEIPT.contains(&key.as_str()),
            );
        }
    }
    if widened.is_empty() {
        BindingDrift::Compatible(compatible)
    } else {
        widened.extend(compatible);
        BindingDrift::NeedsConfirm(widened)
    }
}

fn binding_in(conn: &impl super::StoreConn, install: &str, id: &str) -> Result<Value> {
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
    conn: &impl super::StoreConn,
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
    /// CAD-796: drop every derived-grant row recorded for one
    /// installation. A scope survives only while some remaining
    /// derivation (any app, any install) still covers it, so a rebind
    /// never cuts another install's grant and never keeps this one's.
    pub(super) fn app_install_grants_drop_in(
        tx: &impl super::StoreConn,
        install_id: &str,
        by: &str,
    ) -> Result<()> {
        let apps: Vec<String> = tx.query_vec(
            "SELECT DISTINCT app FROM app_grants WHERE install_id=?",
            [install_id],
            |r| r.get(0),
        )?;
        for app in apps {
            let raws: Vec<(String, String, String, String)> = tx.query_vec(
                "SELECT agent, platform, account, scopes FROM app_grants WHERE app=? AND install_id=?",
                params![app, install_id],
                |r| {
                    let raw: String = r.get(3)?;
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        raw,
                    ))
                },
            )?;
            let mut rows: Vec<(String, String, String, Vec<String>)> = Vec::new();
            for (agent, plat, account, raw) in raws {
                let scopes: Vec<String> = serde_json::from_str(&raw)
                    .map_err(|_| Error::internal("derived grant receipt is corrupt"))?;
                rows.push((agent, plat, account, scopes));
            }
            if rows.is_empty() {
                continue;
            }
            tx.execute(
                "DELETE FROM app_grants WHERE app=? AND install_id=?",
                params![app, install_id],
            )?;
            for (agent, plat, account, derived) in &rows {
                let still: Vec<String> = {
                    let raws = tx.query_vec(
                        "SELECT scopes FROM app_grants WHERE agent=? AND platform=? AND account=?",
                        params![agent, plat, account],
                        |r| r.get::<_, String>(0),
                    )?;
                    let mut still: Vec<String> = Vec::new();
                    for raw in raws {
                        still
                            .extend(serde_json::from_str::<Vec<String>>(&raw).map_err(|_| {
                                Error::internal("derived grant receipt is corrupt")
                            })?);
                    }
                    still
                };
                let existing: Option<String> = tx.query_opt(
                    "SELECT scopes FROM platform_grants WHERE agent=? AND platform=? AND account=?",
                    params![agent, plat, account],
                    |r| r.get(0),
                )?;
                let Some(raw) = existing else {
                    continue;
                };
                let held: Vec<String> = serde_json::from_str(&raw)
                    .map_err(|_| Error::internal("platform grant receipt is corrupt"))?;
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

    /// CAD-796: close the waiting effects of every configured binding on
    /// one credential and drop the affected installations' derived grants.
    /// Called inside the credential revoke/rotate transaction, so the record
    /// change and the closures land together. CAD-1119: the slot itself
    /// stops at the binding receipt (the credential row or its revision no
    /// longer matches) until the operator binds it again; the
    /// installation's approval is the install consent and stays.
    pub(super) fn app_binding_approvals_withdraw_for_credential_in(
        tx: &impl super::StoreConn,
        platform_name: &str,
        account: &str,
    ) -> Result<()> {
        let rows: Vec<(String, String)> = tx.query_vec(
            "SELECT install_id, id FROM app_bindings WHERE state='configured' AND json_extract(config,'$.provider')=? AND json_extract(config,'$.account')=?",
            params![platform_name, account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
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
        self.read_tx(|conn| {
            let ids: Vec<String> = conn.query_vec(
                "SELECT id FROM app_bindings WHERE install_id=? AND state='configured' ORDER BY id",
                [install],
                |row| row.get::<_, String>(0),
            )?;
            ids.iter()
                .map(|id| binding_in(&*conn, install, id))
                .collect()
        })
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
        self.read_tx(|conn| {

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
        })
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
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let existing = tx
                        .query_opt(
                            "SELECT id FROM app_bindings WHERE install_id=? AND request_id=?",
                            params![install, request],
                            |r| r.get::<_, String>(0),
                        )?;
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
                    Ok(json!({"binding":row}))
        })
    }

    pub fn app_binding_update(
        &self,
        install: &str,
        id: &str,
        expected: i64,
        config: &Value,
    ) -> Result<Value> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
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
        // CAD-796: an unchanged re-save is a complete no-op — same
        // revision, open effects, standing approval. Only a material
        // rebind (CAD-692) bumps the incarnation and closes the waiting
        // effects pinned to the old one.
        if row["config"] == *config {
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
            Self::app_install_grants_drop_in(&tx, install, "operator")?;
        }
        Self::event(
            &tx,
            "app_bindings",
            "app_binding_configured",
            json!({"install_id":install,"binding_id":id,"revision":revision+1,"digest":digest}),
        )?;
        let next = binding_in(&tx, install, id)?;
        Ok(json!({"binding":next}))
        })
    }

    /// CAD-1119: carry a binding onto its connection's re-derived receipt
    /// without the operator. The daemon calls this only after proving the
    /// slot contract (capability, version, action, resource kind, effect,
    /// semantics, scopes and contracts) and the connection identity are
    /// unchanged. The row must still be exactly `proof` (compare-and-swap
    /// on id, revision and digest), so a concurrent rebind or revoke wins.
    /// Waiting effects pinned to the old incarnation close: a per-post
    /// approval never carries across a receipt change.
    pub fn app_binding_migrate(
        &self,
        install: &str,
        proof: &BindingProof,
        config: &Value,
    ) -> Result<BindingProof> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
        let row = binding_in(&tx, install, &proof.id)?;
        if row["state"] != "configured"
            || row["revision"] != proof.revision
            || row["digest"] != proof.digest.as_str()
            || row["config"] != proof.config
        {
            return Err(Error::rejected(
                "publication binding revision or incarnation changed",
            ));
        }
        let context = row["context_id"].as_str();
        validate_config(install, context, config)?;
        let BindingDrift::Compatible(changes) = binding_drift(&proof.config, config) else {
            return Err(Error::rejected(
                "binding change widens the slot contract; the operator must confirm it",
            ));
        };
        let slot = row["slot"]
            .as_str()
            .ok_or_else(|| Error::internal("invalid binding slot"))?;
        let digest = config_digest(install, context, slot, config);
        let updated = tx.execute("UPDATE app_bindings SET config=?,digest=?,revision=revision+1,updated=? WHERE install_id=? AND id=? AND revision=? AND state='configured'",params![config.to_string(),digest,now(),install,proof.id,proof.revision])?;
        if updated != 1 {
            return Err(Error::rejected("binding migration lost its revision claim"));
        }
        Self::app_effect_invalidate_in(&tx, install, None, Some(&proof.id), None)?;
        Self::event(
            &tx,
            "app_bindings",
            "app_binding_migrated",
            json!({"install_id":install,"binding_id":proof.id,"revision":proof.revision+1,
                   "digest":digest,"from_digest":proof.digest,"changes":changes,
                   "actor":"daemon"}),
        )?;
        let next = binding_in(&tx, install, &proof.id)?;
        Ok(BindingProof {
            id: proof.id.clone(),
            revision: next["revision"]
                .as_i64()
                .ok_or_else(|| Error::internal("invalid binding revision"))?,
            digest,
            config: config.clone(),
        })
        })
    }

    /// CAD-1328: an operator-proven context save moves every configured
    /// binding of that context onto the new `{id, install_id, revision,
    /// digest}` pin, in the caller's transaction (the context row is already
    /// updated). Only the `context` field of the receipt changes: the pin
    /// exists to catch a changed connection, bundle, descriptor or sink, and
    /// those fields stay as stored, so `binding_drift` still refuses them.
    /// Derived grants rest on the connection, not the context, so they stay.
    /// Waiting effects pinned to the old incarnation fail at send time on
    /// the binding revision check. The only caller is the operator-gated
    /// context update; no other path re-pins.
    pub(super) fn app_bindings_repin_context_in(
        tx: &impl super::StoreConn,
        install: &str,
        context: &str,
    ) -> Result<()> {
        let shown = Self::app_context_show_in(tx, install, context)?;
        if shown["state"] != "active" {
            return Ok(());
        }
        let pin = json!({"id":context,"install_id":install,
            "revision":shown["revision"],"digest":shown["digest"]});
        let ids: Vec<String> = tx.query_vec(
            "SELECT id FROM app_bindings WHERE install_id=? AND context_id=? AND state='configured'",
            params![install, context],
            |r| r.get(0),
        )?;
        for id in ids {
            let row = binding_in(tx, install, &id)?;
            if row["config"]["context"] == pin {
                continue;
            }
            let mut config = row["config"].clone();
            config["context"] = pin.clone();
            validate_config(install, Some(context), &config)?;
            let slot = row["slot"]
                .as_str()
                .ok_or_else(|| Error::internal("invalid binding slot"))?;
            let digest = config_digest(install, Some(context), slot, &config);
            let updated = tx.execute("UPDATE app_bindings SET config=?,digest=?,revision=revision+1,updated=? WHERE install_id=? AND id=? AND revision=? AND state='configured'",params![config.to_string(),digest,now(),install,id,row["revision"].as_i64()])?;
            if updated != 1 {
                return Err(Error::rejected("binding re-pin lost its revision claim"));
            }
            Self::event(
                tx,
                "app_bindings",
                "app_binding_repinned",
                json!({"install_id":install,"binding_id":id,"slot":slot,
                       "revision":row["revision"].as_i64().unwrap_or(0)+1,"digest":digest,
                       "from_digest":row["digest"],"from_context":row["config"]["context"],
                       "to_context":pin,"actor":"operator"}),
            )?;
        }
        Ok(())
    }

    /// CAD-1328: the operator's destination choice re-pins the context's
    /// bindings the same way a context save does (see above).
    pub fn app_context_repin(&self, install: &str, context: &str) -> Result<()> {
        self.write_tx(|conn| Self::app_bindings_repin_context_in(&*conn, install, context))
    }

    /// CAD-1140: carry a configured binding onto an upgraded bundle only
    /// when the upgrade journal staged its exact receipt after verifying the
    /// slot declaration is byte-identical and pinned to the replaced bundle.
    /// The caller runs this within the journaled delivery transaction. CAS
    /// skips rows changed since staging; the current-receipt check skips rows
    /// whose connection is no longer live.
    pub fn app_binding_upgrade_carry(
        &self,
        install: &str,
        bundle_digest: &str,
        workspace: &str,
        carried: &std::collections::BTreeMap<String, Value>,
    ) -> Result<Vec<String>> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let mut done = Vec::with_capacity(carried.len());
            let mut skipped = Vec::new();
            for (id, expected) in carried {
                let moved = match binding_in(&tx, install, id) {
                    Ok(row) => {
                        row["state"] != "configured"
                            || row["config"] != *expected
                            || expected["bundle_digest"].as_str() == Some(bundle_digest)
                    }
                    Err(_) => true,
                };
                if moved {
                    skipped.push(id.clone());
                    continue;
                }
                let row = binding_in(&tx, install, id)?;
                let context = row["context_id"].as_str();
                let slot = row["slot"]
                    .as_str()
                    .ok_or_else(|| Error::internal("invalid binding slot"))?;
                let proof = BindingProof {
                    id: id.clone(),
                    revision: row["revision"]
                        .as_i64()
                        .ok_or_else(|| Error::internal("invalid binding revision"))?,
                    digest: row["digest"]
                        .as_str()
                        .ok_or_else(|| Error::internal("invalid binding digest"))?
                        .to_string(),
                    config: row["config"].clone(),
                };
                if !binding_current_in(&tx, install, context, slot, &proof)? {
                    skipped.push(id.clone());
                    continue;
                }
                let mut config = expected.clone();
                config["bundle_digest"] = json!(bundle_digest);
                validate_config(install, context, &config)?;
                let digest = config_digest(install, context, slot, &config);
                let updated = tx.execute("UPDATE app_bindings SET config=?,digest=?,revision=revision+1,updated=? WHERE install_id=? AND id=? AND revision=? AND state='configured'",params![config.to_string(),digest,now(),install,id,row["revision"].as_i64().unwrap_or(0)])?;
                if updated != 1 {
                    return Err(Error::rejected(
                        "binding bundle carry lost its revision claim",
                    ));
                }
                let next = binding_in(&tx, install, id)?;
                if next["context_id"] != row["context_id"]
                    || next["slot"] != row["slot"]
                    || next["config"]["workspace_id"] != json!(workspace)
                {
                    return Err(Error::rejected(
                        "binding bundle carry changed the receipt's scope",
                    ));
                }
                Self::app_effect_invalidate_in(&tx, install, None, Some(id), None)?;
                Self::event(
                    &tx,
                    "app_bindings",
                    "app_binding_migrated",
                    json!({"install_id":install,"binding_id":id,
                           "revision":row["revision"].as_i64().unwrap_or(0)+1,
                           "digest":digest,"from_digest":row["digest"],
                           "changes":[json!({"field":"bundle_digest","from":expected["bundle_digest"],"to":bundle_digest})],
                           "actor":"daemon","via":"upgrade"}),
                )?;
                done.push(id.clone());
            }
            if !skipped.is_empty() {
                Self::event(
                    &tx,
                    "app_bindings",
                    "app_binding_carry_skipped",
                    json!({"install_id":install,"skipped":skipped,"actor":"daemon","via":"upgrade"}),
                )?;
            }
            Ok(done)
        })
    }

    pub fn app_binding_revoke(&self, install: &str, id: &str, expected: i64) -> Result<Value> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
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
        Self::app_install_grants_drop_in(&tx, install, "operator")?;
        Self::event(
            &tx,
            "app_bindings",
            "app_binding_revoked",
            json!({"install_id":install,"binding_id":id,"revision":revision+1}),
        )?;
        let next = binding_in(&tx, install, id)?;
        Ok(json!({"binding":next}))
        })
    }

    /// History is authoritative even across package versions and revocation.
    pub fn app_binding_slot_recorded(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
    ) -> Result<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM app_bindings WHERE install_id=? AND scope_key=? AND slot=?)",
            params![install, scope_key(context), slot], |row| row.get(0),
        )?)
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

    /// CAD-1508: `proof` is an earlier incarnation of `current` reached only
    /// by approved upgrade carries. Each carry rewrites `bundle_digest` and
    /// nothing else. Any other move, a gap, or a malformed carry record
    /// returns `Ok(false)` — the caller keeps refusing.
    pub(crate) fn app_binding_upgrade_carried(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        proof: &BindingProof,
        current: &BindingProof,
    ) -> Result<bool> {
        if proof.id != current.id
            || proof.revision <= 0
            || proof.revision >= current.revision
            || config_digest(install, context, slot, &proof.config) != proof.digest
            || config_digest(install, context, slot, &current.config) != current.digest
            || !same_receipt_except_bundle(&proof.config, &current.config)
        {
            return Ok(false);
        }
        let Some(proof_bundle) = canonical_digest(proof.config["bundle_digest"].as_str()) else {
            return Ok(false);
        };
        let Some(current_bundle) = canonical_digest(current.config["bundle_digest"].as_str())
        else {
            return Ok(false);
        };
        let payloads: Vec<String> = self.conn().query_vec(
            "SELECT payload FROM events WHERE alias='app_bindings' AND kind='app_binding_migrated'
             AND json_extract(payload,'$.install_id')=?1
             AND json_extract(payload,'$.binding_id')=?2
             ORDER BY seq ASC",
            params![install, proof.id],
            |row| row.get(0),
        )?;
        let mut links = Vec::new();
        for payload in &payloads {
            match parse_upgrade_carry(payload, install, &proof.id) {
                CarryParse::Other => {}
                CarryParse::Corrupt => return Ok(false),
                CarryParse::Link(link) => links.push(link),
            }
        }
        if links.len() > UPGRADE_CARRY_MAX {
            return Ok(false);
        }
        // One carry per revision. A repeated from-digest is legitimate when
        // a later upgrade returns to an earlier bundle: the binding digest
        // is a pure function of the receipt, so the same receipt can be
        // left twice. Two carries at one revision is a fork.
        let mut revisions: Vec<i64> = links.iter().map(|link| link.revision).collect();
        revisions.sort_unstable();
        if revisions.windows(2).any(|pair| pair[0] == pair[1])
            || links.iter().any(|link| link.revision > current.revision)
        {
            return Ok(false);
        }
        let mut cursor_digest = proof.digest.clone();
        let mut cursor_rev = proof.revision;
        let mut cursor_bundle = proof_bundle.to_string();
        for _ in 0..links.len() {
            if cursor_digest == current.digest
                && cursor_rev == current.revision
                && cursor_bundle == current_bundle
            {
                break;
            }
            let Some(next_rev) = cursor_rev.checked_add(1) else {
                return Ok(false);
            };
            let Some(link) = links
                .iter()
                .find(|link| link.from_digest == cursor_digest && link.revision == next_rev)
            else {
                return Ok(false);
            };
            if link.from_bundle != cursor_bundle {
                return Ok(false);
            }
            cursor_digest = link.digest.clone();
            cursor_rev = next_rev;
            cursor_bundle = link.to_bundle.clone();
        }
        // Carries at or before the proof are earlier history. Every later
        // carry has a unique revision at or below the live row, and the
        // walk consumes proof+1..=current, so a side carry cannot hide.
        Ok(cursor_digest == current.digest
            && cursor_rev == current.revision
            && cursor_bundle == current_bundle)
    }
}

const UPGRADE_CARRY_MAX: usize = 32;

struct UpgradeCarryLink {
    from_digest: String,
    digest: String,
    revision: i64,
    from_bundle: String,
    to_bundle: String,
}

enum CarryParse {
    Other,
    Corrupt,
    Link(UpgradeCarryLink),
}

fn canonical_digest(digest: Option<&str>) -> Option<&str> {
    let digest = digest?;
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    .then_some(digest)
}

fn same_receipt_except_bundle(proof: &Value, current: &Value) -> bool {
    if !proof.is_object() || !current.is_object() {
        return false;
    }
    let mut expected = proof.clone();
    expected["bundle_digest"] = current["bundle_digest"].clone();
    expected == *current
}

fn parse_upgrade_carry(payload: &str, install: &str, binding_id: &str) -> CarryParse {
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return CarryParse::Corrupt;
    };
    if value.get("via") != Some(&json!("upgrade")) {
        return CarryParse::Other;
    }
    let Some(from_digest) = canonical_digest(value["from_digest"].as_str()) else {
        return CarryParse::Corrupt;
    };
    let Some(digest) = canonical_digest(value["digest"].as_str()) else {
        return CarryParse::Corrupt;
    };
    let Some(revision) = value["revision"].as_i64().filter(|revision| *revision > 0) else {
        return CarryParse::Corrupt;
    };
    let Some(changes) = value["changes"].as_array() else {
        return CarryParse::Corrupt;
    };
    let Some(change) = changes.first().filter(|_| changes.len() == 1) else {
        return CarryParse::Corrupt;
    };
    let Some(from_bundle) = canonical_digest(change["from"].as_str()) else {
        return CarryParse::Corrupt;
    };
    let Some(to_bundle) = canonical_digest(change["to"].as_str()) else {
        return CarryParse::Corrupt;
    };
    if value["actor"] != "daemon"
        || value["install_id"] != install
        || value["binding_id"] != binding_id
        || change["field"] != "bundle_digest"
        || from_digest == digest
        || from_bundle == to_bundle
    {
        return CarryParse::Corrupt;
    }
    CarryParse::Link(UpgradeCarryLink {
        from_digest: from_digest.to_string(),
        digest: digest.to_string(),
        revision,
        from_bundle: from_bundle.to_string(),
        to_bundle: to_bundle.to_string(),
    })
}
