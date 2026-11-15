//! Agent ingress endpoints (CAD-1857).
//!
//! An endpoint is one agent's receiving identity and binds exactly one
//! workflow. It is created disabled and unverified; `verify` and `enable`
//! are separate operator steps. The workflow source digest is pinned at
//! creation, so a later workflow edit cannot change what an already-created
//! endpoint means without a new endpoint or an explicit re-pin (a follow-up).
//!
//! This module is only the registry. Ingress, wake, address/secret
//! allocation and the v2 projection are separate tickets.

use crate::error::{Error, Result};
use rusqlite::params;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{now, Store, StoreConn};

/// The registry table. New table only; no existing row is rewritten.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agent_endpoints (
    id TEXT PRIMARY KEY,
    agent_uid TEXT NOT NULL,
    kind TEXT NOT NULL,
    slug TEXT NOT NULL,
    state TEXT NOT NULL,
    verified INTEGER NOT NULL DEFAULT 0,
    project TEXT NOT NULL,
    workflow TEXT NOT NULL,
    digest TEXT NOT NULL,
    created REAL NOT NULL,
    updated REAL NOT NULL,
    UNIQUE(kind, slug)
);
CREATE INDEX IF NOT EXISTS agent_endpoints_agent ON agent_endpoints(agent_uid);
";

const BY_ID: &str = "SELECT e.id, e.agent_uid, a.alias AS agent_alias, e.kind, \
    e.slug, e.state, e.verified, e.project, e.workflow, e.digest, \
    e.created, e.updated \
    FROM agent_endpoints e LEFT JOIN agents a ON a.uid = e.agent_uid \
    WHERE e.id=?";

const ALL: &str = "SELECT e.id, e.agent_uid, a.alias AS agent_alias, e.kind, \
    e.slug, e.state, e.verified, e.project, e.workflow, e.digest, \
    e.created, e.updated \
    FROM agent_endpoints e LEFT JOIN agents a ON a.uid = e.agent_uid \
    ORDER BY e.created, e.id";

/// One configured endpoint. The alias is the current name of the agent
/// that owns `agent_uid`; it is null when that agent row no longer exists,
/// which is why the endpoint is bound to the uid, never the alias.
#[derive(Debug, Clone)]
pub struct AgentEndpoint {
    pub id: String,
    pub agent_uid: String,
    pub agent_alias: Option<String>,
    pub kind: String,
    pub slug: String,
    pub state: String,
    pub verified: bool,
    pub project: String,
    pub workflow: String,
    pub digest: String,
    pub created: f64,
    pub updated: f64,
}

impl AgentEndpoint {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "agent": self.agent_alias,
            "agent_uid": self.agent_uid,
            "kind": self.kind,
            "slug": self.slug,
            "state": self.state,
            "verified": self.verified,
            "project": self.project,
            "workflow": self.workflow,
            "digest": self.digest,
            "created": self.created,
            "updated": self.updated,
        })
    }
}

fn row_endpoint(row: &rusqlite::Row) -> rusqlite::Result<AgentEndpoint> {
    Ok(AgentEndpoint {
        id: row.get("id")?,
        agent_uid: row.get("agent_uid")?,
        agent_alias: row.get("agent_alias")?,
        kind: row.get("kind")?,
        slug: row.get("slug")?,
        state: row.get("state")?,
        verified: row.get::<_, i64>("verified")? != 0,
        project: row.get("project")?,
        workflow: row.get("workflow")?,
        digest: row.get("digest")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

fn validate_kind(kind: &str) -> Result<()> {
    if !matches!(kind, "email" | "webhook" | "schedule") {
        return Err(Error::rejected(
            "Endpoint kind must be email, webhook or schedule",
        ));
    }
    Ok(())
}

/// The address/URL local part. Twenty characters keeps an email local part
/// (endpoint + dot + company, company <= 42) inside the RFC 5321 64-octet
/// limit and stays readable in a URL path.
fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug.len() > 20 {
        return Err(Error::rejected(
            "Endpoint slug must contain 1-20 characters",
        ));
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(Error::rejected(
            "Endpoint slug may contain only lowercase letters, digits and hyphens",
        ));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(Error::rejected(
            "Endpoint slug may not start or end with a hyphen",
        ));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<()> {
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::rejected(
            "Endpoint digest must be a sha256:<64 hex> workflow source digest",
        ));
    }
    Ok(())
}

impl Store {
    fn endpoint_in(&self, conn: &impl StoreConn, id: &str) -> Result<AgentEndpoint> {
        conn.query_opt(BY_ID, params![id], row_endpoint)?
            .ok_or_else(|| Error::rejected(format!("Unknown endpoint '{id}'")))
    }

    /// Create a disabled, unverified endpoint bound to the agent's durable
    /// uid and the workflow source digest. The workflow itself is resolved
    /// by the caller (it owns the PM path); this layer refuses an empty
    /// binding or a malformed digest.
    pub fn endpoint_add(
        &self,
        agent: &str,
        kind: &str,
        slug: &str,
        project: &str,
        workflow: &str,
        digest: &str,
    ) -> Result<AgentEndpoint> {
        validate_kind(kind)?;
        validate_slug(slug)?;
        if project.trim().is_empty() || project.len() > 128 {
            return Err(Error::rejected(
                "Endpoint project must contain 1-128 characters",
            ));
        }
        if workflow.trim().is_empty() || workflow.len() > 64 {
            return Err(Error::rejected(
                "Endpoint workflow must contain 1-64 characters",
            ));
        }
        validate_digest(digest)?;
        let agent = self.agent(agent)?;
        let id = format!("ep_{}", Uuid::new_v4().simple());
        let t = now();
        self.write_tx(|tx| {
            if tx
                .query_opt(
                    "SELECT id FROM agent_endpoints WHERE kind=? AND slug=?",
                    params![kind, slug],
                    |row| row.get::<_, String>(0),
                )?
                .is_some()
            {
                return Err(Error::rejected(format!(
                    "Endpoint '{kind}/{slug}' already exists"
                )));
            }
            tx.execute(
                "INSERT INTO agent_endpoints \
                 (id, agent_uid, kind, slug, state, verified, project, workflow, digest, created, updated) \
                 VALUES (?1, ?2, ?3, ?4, 'disabled', 0, ?5, ?6, ?7, ?8, ?8)",
                params![id, agent.uid, kind, slug, project, workflow, digest, t],
            )?;
            self.endpoint_in(&*tx, &id)
        })
    }

    /// All endpoints, optionally filtered by agent alias or state.
    pub fn endpoints(
        &self,
        agent: Option<&str>,
        state: Option<&str>,
    ) -> Result<Vec<AgentEndpoint>> {
        let uid = match agent {
            Some(alias) => Some(self.agent(alias)?.uid),
            None => None,
        };
        self.read_tx(|conn| {
            let rows = conn
                .query_vec(ALL, [], row_endpoint)
                .map(|rows| rows.into_iter().map(Ok::<_, rusqlite::Error>))?;
            let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .filter(|endpoint| {
                    uid.as_ref().is_none_or(|uid| endpoint.agent_uid == *uid)
                        && state.is_none_or(|state| endpoint.state == state)
                })
                .collect())
        })
    }

    pub fn endpoint(&self, id: &str) -> Result<AgentEndpoint> {
        self.read_tx(|conn| self.endpoint_in(&conn, id))
    }

    /// Mark the endpoint verified. It stays disabled until `enable`.
    pub fn endpoint_verify(&self, id: &str) -> Result<AgentEndpoint> {
        self.write_tx(|tx| {
            self.endpoint_in(&*tx, id)?;
            tx.execute(
                "UPDATE agent_endpoints SET verified=1, updated=? WHERE id=?",
                params![now(), id],
            )?;
            self.endpoint_in(&*tx, id)
        })
    }

    /// Enable or disable the endpoint. Enabling requires `verify` first;
    /// disabling is always allowed. The transition is idempotent.
    pub fn endpoint_set_enabled(&self, id: &str, enabled: bool) -> Result<AgentEndpoint> {
        self.write_tx(|tx| {
            let endpoint = self.endpoint_in(&*tx, id)?;
            if enabled && !endpoint.verified {
                return Err(Error::rejected(format!(
                    "Endpoint '{id}' is not verified — run `cadence endpoint verify {id}` first"
                )));
            }
            let state = if enabled { "enabled" } else { "disabled" };
            if endpoint.state != state {
                tx.execute(
                    "UPDATE agent_endpoints SET state=?, updated=? WHERE id=?",
                    params![state, now(), id],
                )?;
            }
            self.endpoint_in(&*tx, id)
        })
    }

    /// Remove the row and return what was removed. Ingress/event references
    /// are a follow-up; the registry itself has no dependent rows yet.
    pub fn endpoint_remove(&self, id: &str) -> Result<AgentEndpoint> {
        self.write_tx(|tx| {
            let endpoint = self.endpoint_in(&*tx, id)?;
            tx.execute("DELETE FROM agent_endpoints WHERE id=?", params![id])?;
            Ok(endpoint)
        })
    }
}
