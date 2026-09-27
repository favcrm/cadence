//! Server-derived accepted-artifact authorization, distinct from worker grants.
use super::*;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::path::Path;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_effect_authorizations(
 effect_id TEXT PRIMARY KEY REFERENCES platform_effects(effect_id),
 install_id TEXT NOT NULL, context_id TEXT, run_id TEXT NOT NULL,
 artifact_id TEXT NOT NULL, authority TEXT NOT NULL, authority_digest TEXT NOT NULL,
 input_digest TEXT NOT NULL, release_digest TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS app_effect_install_scope
 ON app_effect_authorizations(install_id,context_id,effect_id);
";

#[derive(Debug, Clone)]
pub struct AppEffectPermit {
    pub effect_id: String,
    pub input: Value,
    pub input_digest: String,
    pub authority_digest: String,
    pub provenance: Value,
}

pub fn input_digest(input: &Value) -> String {
    app_runs::artifact_digest(input.to_string().as_bytes())
}

fn release_digest(row: &EffectRow, authority: &Value) -> String {
    app_runs::material_digest(&json!({"kind":"app-release-v1","request":row.request,
        "platform":row.platform,"account":row.account,"tool":row.tool,"input":row.input,
        "preview":row.preview,"source_hash":row.source_hash,"scopes":row.scopes,
        "authority":authority}))
}

fn validate_child(row: &EffectRow, authority: &Value) -> Result<()> {
    if !authority.is_object()
        || authority["schema"] != 1
        || !authority["install_id"].is_string()
        || !authority["run_id"].is_string()
        || !authority["artifact_id"].is_string()
        || !authority["provenance"].is_object()
        || row.input["provenance"] != authority["provenance"]
        || row.input["schema"] != 1
        || !row.input["title"].is_string()
        || row.source_name.is_some()
        || row.task.is_some()
        || row.input.get("project").is_some()
        || row.input.get("attachments").is_some()
    {
        return Err(Error::rejected(
            "app effect child does not match its server-owned input",
        ));
    }
    let body = row.input["body"]
        .as_str()
        .ok_or_else(|| Error::rejected("app effect body is not text"))?;
    let digest = app_runs::artifact_digest(body.as_bytes());
    if row.source_hash.as_deref() != Some(digest.as_str())
        || authority["artifact_digest"] != digest
        || authority["provenance"]["artifact_digest"] != digest
        || authority["provenance"]["install_id"] != authority["install_id"]
        || authority["provenance"]["run_id"] != authority["run_id"]
        || authority["provenance"]["artifact_id"] != authority["artifact_id"]
        || serde_json::to_vec(&row.input)?.len() > 64 * 1024
        || row.preview.len() > 16 * 1024
    {
        return Err(Error::rejected(
            "app effect artifact receipt or input/preview bound is invalid",
        ));
    }
    Ok(())
}

fn child_in(conn: &Connection, id: &str) -> Result<(EffectRow, Value, String)> {
    let row = conn
        .query_row(
            "SELECT * FROM platform_effects WHERE effect_id=?",
            [id],
            effects::EffectRow::from_row,
        )
        .optional()?
        .ok_or_else(|| Error::rejected("app effect does not exist"))?;
    let kind: String = conn.query_row(
        "SELECT authorization_kind FROM platform_effects WHERE effect_id=?",
        [id],
        |r| r.get(0),
    )?;
    if kind != "app_artifact" {
        return Err(Error::rejected(
            "effect is not authorized by an app artifact",
        ));
    }
    let child = conn.query_row("SELECT install_id,context_id,run_id,artifact_id,authority,authority_digest,input_digest,release_digest FROM app_effect_authorizations WHERE effect_id=?",[id],|r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?)))
        .optional()?.ok_or_else(|| Error::rejected("app effect authorization child is missing"))?;
    let authority: Value = serde_json::from_str(&child.4)?;
    validate_child(&row, &authority)?;
    if authority["install_id"] != child.0
        || authority["context"]["id"].as_str() != child.1.as_deref()
        || authority["run_id"] != child.2
        || authority["artifact_id"] != child.3
        || app_runs::material_digest(&authority) != child.5
        || input_digest(&row.input) != child.6
        || release_digest(&row, &authority) != child.7
    {
        return Err(Error::rejected(
            "app effect authorization receipt is corrupt",
        ));
    }
    Ok((row, authority, child.7))
}

fn envelope(row: &EffectRow, authority: &Value, digest: &str) -> Value {
    json!({"effect":{"effect_id":row.effect_id,"request":row.request,"state":row.state,
        "digest":digest,"record":row.to_record(),"authority":authority}})
}

/// The provider opens the existing database read-only. It cannot recover a
/// daemon or manufacture an authorization while preparing a filesystem write.
pub fn read_execution_permit(path: &Path, effect_id: &str) -> Result<AppEffectPermit> {
    let conn = open_read_only(path)?;
    let (row, authority, _) = child_in(&conn, effect_id)?;
    if row.state != "executing" {
        return Err(Error::rejected("app effect has no executing claim"));
    }
    Ok(AppEffectPermit {
        effect_id: row.effect_id,
        input_digest: input_digest(&row.input),
        input: row.input,
        authority_digest: app_runs::material_digest(&authority),
        provenance: authority["provenance"].clone(),
    })
}

impl Store {
    pub fn app_effect_show(&self, id: &str) -> Result<Value> {
        let (row, authority, digest) = child_in(&self.conn(), id)?;
        Ok(envelope(&row, &authority, &digest))
    }
    pub fn app_effect_list(&self, install: Option<&str>, context: Option<&str>) -> Result<Value> {
        if context.is_some() && install.is_none() {
            return Err(Error::rejected("context filter requires installation"));
        }
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT effect_id FROM app_effect_authorizations WHERE (? IS NULL OR install_id=?) AND (? IS NULL OR context_id=?) ORDER BY effect_id LIMIT 100")?;
        let ids = stmt
            .query_map(params![install, install, context, context], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut list = Vec::new();
        for id in ids {
            let (row, authority, digest) = child_in(&conn, &id)?;
            list.push(envelope(&row, &authority, &digest)["effect"].clone());
        }
        Ok(json!({"effects":list}))
    }
    pub fn app_effect_is_child(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT authorization_kind='app_artifact' FROM platform_effects WHERE effect_id=?",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// Atomic row/child/audit insertion; retries require the entire frozen
    /// authority and presentation, not merely matching provider arguments.
    pub fn app_effect_stage(&self, row: &EffectRow, authority: &Value) -> Result<Value> {
        validate_child(row, authority)?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let existing = tx
            .query_row(
                "SELECT effect_id FROM platform_effects WHERE request=?",
                [&row.request],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let (existing, stored, digest) = child_in(&tx, &id)?;
            if release_digest(row, authority) != digest || stored != *authority {
                return Err(Error::rejected(
                    "app release request already names different approved material",
                ));
            }
            tx.commit()?;
            return Ok(envelope(&existing, &stored, &digest));
        }
        let digest = release_digest(row, authority);
        tx.execute("INSERT INTO platform_effects(effect_id,request,agent,platform,account,tool,label,input,input_summary,preview,source_name,source_hash,scopes,task,state,staged_at,updated_at,authorization_kind) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,'waiting',?,?,'app_artifact')",
            params![row.effect_id,row.request,row.agent,row.platform,row.account,row.tool,row.label,row.input.to_string(),row.input_summary,row.preview,row.source_name,row.source_hash,serde_json::to_string(&row.scopes)?,row.task,now(),now()])?;
        tx.execute(
            "INSERT INTO app_effect_authorizations VALUES(?,?,?,?,?,?,?,?,?)",
            params![
                row.effect_id,
                authority["install_id"].as_str(),
                authority["context"]["id"].as_str(),
                authority["run_id"].as_str(),
                authority["artifact_id"].as_str(),
                authority.to_string(),
                app_runs::material_digest(authority),
                input_digest(&row.input),
                digest
            ],
        )?;
        Self::event(
            &tx,
            platform::PLATFORM_STREAM,
            EFFECT_REQUESTED_EVENT,
            json!({"effect_id":row.effect_id,"request":row.request,"authorization_kind":"app_artifact","install_id":authority["install_id"],"digest":digest}),
        )?;
        let (stored, authority, digest) = child_in(&tx, &row.effect_id)?;
        tx.commit()?;
        Ok(envelope(&stored, &authority, &digest))
    }

    /// Eligibility and the exactly-one claim share a transaction. A caller
    /// must hold the daemon release lock until provider commit/readback;
    /// this method drops SQLite before any adapter is called.
    pub(crate) fn app_effect_claim<F>(
        &self,
        id: &str,
        digest: &str,
        eligible: F,
    ) -> Result<Option<EffectRow>>
    where
        F: FnOnce(&Connection, &Value) -> Result<bool>,
    {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let (row, authority, stored_digest) = child_in(&tx, id)?;
        if digest != stored_digest {
            return Err(Error::rejected("app effect release digest changed"));
        }
        if row.state != "decided" || !eligible(&tx, &authority)? {
            return Ok(None);
        }
        let count = tx.execute("UPDATE platform_effects SET state='executing',updated_at=? WHERE effect_id=? AND state='decided' AND authorization_kind='app_artifact'",params![now(),id])?;
        if count != 1 {
            return Ok(None);
        }
        let mut claimed = row;
        claimed.state = "executing".into();
        tx.commit()?;
        Ok(Some(claimed))
    }
}
