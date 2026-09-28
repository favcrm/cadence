//! Operator-owned bounded content defaults. These confer no execution authority.
use super::*;
use crate::store::app_runs::material_digest;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const CONFIG_BYTES: usize = 32 * 1024;
pub const CONTEXT_LIMIT: i64 = 100;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_contexts(
 id TEXT PRIMARY KEY, install_id TEXT NOT NULL, request_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 state TEXT NOT NULL CHECK(state IN ('active','archived')),
 config_json TEXT NOT NULL, config_digest TEXT NOT NULL,
 created REAL NOT NULL, updated REAL NOT NULL, UNIQUE(install_id,request_id));
";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    pub schema: u32,
    pub label: String,
    pub input_defaults: BTreeMap<String, String>,
}
impl ContextConfig {
    pub fn new(label: &str, input_defaults: BTreeMap<String, String>) -> Result<Self> {
        let config = Self {
            schema: 1,
            label: label.trim().to_string(),
            input_defaults,
        };
        config.validate()?;
        Ok(config)
    }
    fn validate(&self) -> Result<()> {
        if self.schema != 1
            || self.label.is_empty()
            || self.label.len() > 120
            || self.label.trim() != self.label
            || self.label.chars().any(char::is_control)
            || serde_json::to_vec(self)
                .map_err(|e| Error::internal(e.to_string()))?
                .len()
                > CONFIG_BYTES
        {
            return Err(Error::rejected(
                "context configuration exceeds its supported shape or bounds",
            ));
        }
        Ok(())
    }
    fn digest(&self, install_id: &str) -> Result<String> {
        self.validate()?;
        Ok(material_digest(
            &json!({"domain":"cadence-app-context-v1","install_id":install_id,"config":self}),
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextProof {
    pub id: String,
    pub install_id: String,
    pub revision: i64,
    pub digest: String,
}

impl Store {
    pub(super) fn app_context_show_in(conn: &Connection, install: &str, id: &str) -> Result<Value> {
        let (revision,state,encoded,digest): (i64,String,String,String) = conn.query_row(
            "SELECT revision,state,config_json,config_digest FROM app_contexts WHERE id=? AND install_id=?",
            params![id,install], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))
        ).optional()?.ok_or_else(|| Error::rejected("context is unavailable for this installation"))?;
        if encoded.len() > CONFIG_BYTES || revision < 1 {
            return Err(Error::rejected("context integrity refused"));
        }
        let config: ContextConfig = serde_json::from_str(&encoded)
            .map_err(|_| Error::rejected("context integrity refused"))?;
        if config.digest(install)? != digest {
            return Err(Error::rejected("context integrity refused"));
        }
        Ok(
            json!({"id":id,"install_id":install,"revision":revision,"digest":digest,"state":state,"config":config}),
        )
    }
    pub fn app_context_show(&self, install: &str, id: &str) -> Result<Value> {
        Ok(json!({"context":Self::app_context_show_in(&self.conn(),install,id)?}))
    }
    pub fn app_context_list(&self, install: &str) -> Result<Value> {
        let conn = self.conn();
        let ids = conn
            .prepare(
                "SELECT id FROM app_contexts WHERE install_id=? ORDER BY created,id LIMIT 101",
            )?
            .query_map([install], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.len() > CONTEXT_LIMIT as usize {
            return Err(Error::rejected(
                "context inventory exceeds its supported bound",
            ));
        }
        Ok(
            json!({"contexts":ids.iter().map(|id|Self::app_context_show_in(&conn,install,id)).collect::<Result<Vec<_>>>()?}),
        )
    }
    pub fn app_context_proof(
        &self,
        install: &str,
        id: &str,
    ) -> Result<(ContextConfig, ContextProof)> {
        let row = Self::app_context_show_in(&self.conn(), install, id)?;
        if row["state"] != "active" {
            return Err(Error::rejected("context is archived"));
        }
        let config = serde_json::from_value(row["config"].clone())
            .map_err(|_| Error::rejected("context integrity refused"))?;
        Ok((
            config,
            ContextProof {
                id: id.to_string(),
                install_id: install.to_string(),
                revision: row["revision"].as_i64().unwrap(),
                digest: row["digest"].as_str().unwrap().to_string(),
            },
        ))
    }
    pub(super) fn app_context_proof_current_in(
        conn: &Connection,
        install: &str,
        proof: &ContextProof,
    ) -> Result<()> {
        if proof.install_id != install {
            return Err(Error::rejected("context installation proof is stale"));
        }
        let row = Self::app_context_show_in(conn, install, &proof.id)?;
        if row["state"] != "active"
            || row["revision"].as_i64() != Some(proof.revision)
            || row["digest"].as_str() != Some(proof.digest.as_str())
        {
            return Err(Error::rejected(
                "context revision or configuration is stale",
            ));
        }
        Ok(())
    }
    pub fn app_context_create(
        &self,
        install: &str,
        config: &ContextConfig,
        request: &str,
    ) -> Result<Value> {
        crate::proto::identifier(install, "installation ID")?;
        crate::proto::identifier(request, "context request ID")?;
        let digest = config.digest(install)?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM app_contexts WHERE install_id=? AND request_id=?",
                params![install, request],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            let existing = Self::app_context_show_in(&tx, install, &id)?;
            if existing["digest"].as_str() != Some(digest.as_str()) || existing["state"] != "active"
            {
                return Err(Error::rejected(
                    "context request ID already has different or archived configuration",
                ));
            }
            return Ok(json!({"context":existing}));
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM app_contexts WHERE install_id=?",
            [install],
            |r| r.get(0),
        )?;
        if count >= CONTEXT_LIMIT {
            return Err(Error::rejected(
                "installation has reached its context limit",
            ));
        }
        let id = format!("ctx-{}", uuid::Uuid::new_v4().simple());
        tx.execute("INSERT INTO app_contexts(id,install_id,request_id,revision,state,config_json,config_digest,created,updated) VALUES(?,?,?,1,'active',?,?,?,?)",params![id,install,request,serde_json::to_string(config).map_err(|e|Error::internal(e.to_string()))?,digest,now(),now()])?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            "app_context_created",
            json!({"context_id":id,"install_id":install,"revision":1,"digest":digest,"actor":"operator"}),
        )?;
        let result = json!({"context":Self::app_context_show_in(&tx,install,&id)?});
        tx.commit()?;
        Ok(result)
    }
    pub fn app_context_update(
        &self,
        install: &str,
        id: &str,
        expected: i64,
        config: &ContextConfig,
    ) -> Result<Value> {
        self.app_context_change(install, id, expected, Some(config))
    }
    pub fn app_context_archive(&self, install: &str, id: &str, expected: i64) -> Result<Value> {
        self.app_context_change(install, id, expected, None)
    }
    fn app_context_change(
        &self,
        install: &str,
        id: &str,
        expected: i64,
        config: Option<&ContextConfig>,
    ) -> Result<Value> {
        if expected < 1 {
            return Err(Error::rejected(
                "expected context revision must be positive",
            ));
        }
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let row = Self::app_context_show_in(&tx, install, id)?;
        if row["revision"].as_i64() != Some(expected) {
            return Err(Error::rejected("context revision is stale"));
        }
        if row["state"] == "archived" {
            return if config.is_none() {
                Ok(json!({"context":row}))
            } else {
                Err(Error::rejected("archived context cannot be updated"))
            };
        }
        let revision = expected
            .checked_add(1)
            .ok_or_else(|| Error::rejected("context revision exhausted"))?;
        if let Some(config) = config {
            tx.execute("UPDATE app_contexts SET revision=?,config_json=?,config_digest=?,updated=? WHERE id=? AND install_id=? AND revision=? AND state='active'",params![revision,serde_json::to_string(config).map_err(|e|Error::internal(e.to_string()))?,config.digest(install)?,now(),id,install,expected])?;
        } else {
            tx.execute("UPDATE app_contexts SET revision=?,state='archived',updated=? WHERE id=? AND install_id=? AND revision=? AND state='active'",params![revision,now(),id,install,expected])?;
        }
        let runs = tx.prepare("SELECT id FROM app_runs WHERE install_id=? AND context_id=? AND state IN ('awaiting_approval','approved','running')")?
            .query_map(params![install,id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for run in runs {
            self.app_run_invalidate_in(&tx, &run)?;
        }
        Self::app_effect_invalidate_in(&tx, install, Some(id), None, None)?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            if config.is_some() {
                "app_context_updated"
            } else {
                "app_context_archived"
            },
            json!({"context_id":id,"install_id":install,"revision":revision,"actor":"operator"}),
        )?;
        let result = json!({"context":Self::app_context_show_in(&tx,install,id)?});
        tx.commit()?;
        Ok(result)
    }
}
