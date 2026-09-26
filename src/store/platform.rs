//! CAD-366 / ADR 0006 §5.1, §5.3, §5.5: platform credential records,
//! per-agent scope grants and per-project default accounts.
//!
//! These tables hold HANDLES ONLY. Credential bytes live in
//! [`crate::platform::custody`], never in a row — the record a verb or
//! event returns is `{platform, account, scopes, fingerprint,
//! enrolled_at, by}` where `fingerprint` is the SHA-256 prefix
//! [`crate::secret`] puts on findings.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::proto::identifier;

use super::{now, Store};

/// The platform custody audit stream (§5.5). Like `audit:approvals`
/// the name is no valid agent identifier (it holds a `:`), so no
/// `agent rm` deletes it and no retention prune names it; the events
/// carry fingerprints and handles, never a credential.
pub const PLATFORM_STREAM: &str = "audit:platforms";

/// §5.5 event names.
pub const PLATFORM_CONNECTED_EVENT: &str = "platform_connected";
pub const PLATFORM_DISCONNECTED_EVENT: &str = "platform_disconnected";
pub const SCOPE_GRANTED_EVENT: &str = "scope_granted";
pub const SCOPE_REVOKED_EVENT: &str = "scope_revoked";
pub const CREDENTIAL_REVOKED_EVENT: &str = "credential_revoked";
/// Not in §5.5's named set: the project default is metadata, recorded
/// for the same audit story.
pub const PLATFORM_DEFAULT_EVENT: &str = "platform_default_set";

/// CAD-577: the derived app grant events. An app's approval derives
/// exactly the scopes its workflow steps declare on their bound slots,
/// to the agents assigned to those steps; these events record the
/// derivation and its revocation on the same audit stream.
pub const APP_GRANTED_EVENT: &str = "app_granted";
pub const APP_GRANTS_REVOKED_EVENT: &str = "app_grants_revoked";

/// A custody record — what `platform_accounts` and every event carry.
/// No field holds credential bytes; `custody` names the backend the
/// bytes live in (ADR 0006 §5.3).
#[derive(Clone, Debug)]
pub struct CredentialRecord {
    pub platform: String,
    pub account: String,
    pub scopes: Vec<String>,
    pub fingerprint: String,
    /// The custody backend tag — `file`, or a keychain name.
    pub custody: String,
    /// The exchange shape that enrolled it — `token` or `consent`.
    pub exchange: String,
    pub enrolled_at: f64,
    pub by: String,
}

impl CredentialRecord {
    /// The ADR record plus the custody/exchange handles — the only
    /// shape an RPC result or audit event ever renders.
    pub fn to_json(&self) -> Value {
        json!({
            "platform": self.platform,
            "account": self.account,
            "scopes": self.scopes,
            "fingerprint": self.fingerprint,
            "custody": self.custody,
            "exchange": self.exchange,
            "enrolled_at": self.enrolled_at,
            "by": self.by,
        })
    }
}

/// A per-agent scope grant (§5.3): `(agent, platform, account,
/// scopes)` — what the grant check consults before any platform
/// traffic.
#[derive(Clone, Debug, PartialEq)]
pub struct Grant {
    pub agent: String,
    pub platform: String,
    pub account: String,
    pub scopes: Vec<String>,
    pub granted_at: f64,
    pub by: String,
}

impl Grant {
    pub fn to_json(&self) -> Value {
        json!({
            "agent": self.agent,
            "platform": self.platform,
            "account": self.account,
            "scopes": self.scopes,
            "granted_at": self.granted_at,
            "by": self.by,
        })
    }

    /// Does this grant cover `scope` — exact match, or `*` (the
    /// operator's whole-account grant).
    pub fn covers(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == "*" || s == scope)
    }
}

/// A project's default account for one platform (§5.1: "a project
/// names a default account").
#[derive(Clone, Debug)]
pub struct ProjectDefault {
    pub project: String,
    pub platform: String,
    pub account: String,
    pub set_at: f64,
    pub by: String,
}

/// The v17 schema objects. `IF NOT EXISTS` so a half-applied
/// migration converges on reopen.
pub(super) const SCHEMA_V17: &str = "CREATE TABLE IF NOT EXISTS platform_credentials(
        platform TEXT NOT NULL,
        account TEXT NOT NULL,
        scopes TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        custody TEXT NOT NULL,
        exchange TEXT NOT NULL,
        enrolled_at REAL NOT NULL,
        by TEXT NOT NULL,
        PRIMARY KEY(platform, account));
     CREATE TABLE IF NOT EXISTS platform_grants(
        agent TEXT NOT NULL,
        platform TEXT NOT NULL,
        account TEXT NOT NULL,
        scopes TEXT NOT NULL,
        granted_at REAL NOT NULL,
        by TEXT NOT NULL,
        PRIMARY KEY(agent, platform, account));
     CREATE TABLE IF NOT EXISTS platform_defaults(
        project TEXT NOT NULL,
        platform TEXT NOT NULL,
        account TEXT NOT NULL,
        set_at REAL NOT NULL,
        by TEXT NOT NULL,
        PRIMARY KEY(project, platform));
     CREATE TABLE IF NOT EXISTS app_grants(
        app TEXT NOT NULL,
        agent TEXT NOT NULL,
        platform TEXT NOT NULL,
        account TEXT NOT NULL,
        scopes TEXT NOT NULL,
        granted_at REAL NOT NULL,
        by TEXT NOT NULL,
        PRIMARY KEY(app, agent, platform, account));";

fn scopes_json(scopes: &[String]) -> Result<String> {
    Ok(serde_json::to_string(scopes)?)
}

fn scopes_of(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn credential_row(row: &rusqlite::Row) -> rusqlite::Result<CredentialRecord> {
    let scopes: String = row.get("scopes")?;
    Ok(CredentialRecord {
        platform: row.get("platform")?,
        account: row.get("account")?,
        scopes: scopes_of(&scopes),
        fingerprint: row.get("fingerprint")?,
        custody: row.get("custody")?,
        exchange: row.get("exchange")?,
        enrolled_at: row.get("enrolled_at")?,
        by: row.get("by")?,
    })
}

fn grant_row(row: &rusqlite::Row) -> rusqlite::Result<Grant> {
    let scopes: String = row.get("scopes")?;
    Ok(Grant {
        agent: row.get("agent")?,
        platform: row.get("platform")?,
        account: row.get("account")?,
        scopes: scopes_of(&scopes),
        granted_at: row.get("granted_at")?,
        by: row.get("by")?,
    })
}

/// A scope name: 1-128 chars of `[A-Za-z0-9._:*-]` — the shape a
/// platform's permission vocabulary takes (`workers:write`,
/// `dns:*`). Anything else is refused so a scope stays a token a
/// refusal can name.
pub fn scope_name(value: &str) -> Result<&str> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '*' | '-'));
    if valid {
        Ok(value)
    } else {
        Err(Error::rejected(
            "Scope must be 1-128 chars of [A-Za-z0-9._:*-]",
        ))
    }
}

/// Validate a params `"scopes"` array — a non-empty list of scope
/// names.
pub fn scope_list(params: &Value, field: &str) -> Result<Vec<String>> {
    let list = params
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::rejected(format!("Missing or non-array '{field}'")))?;
    if list.is_empty() {
        return Err(Error::rejected(format!(
            "'{field}' must name at least one scope"
        )));
    }
    let mut scopes = Vec::with_capacity(list.len());
    for s in list {
        let s = s
            .as_str()
            .ok_or_else(|| Error::rejected(format!("'{field}' holds a non-string scope")))?;
        scopes.push(scope_name(s)?.to_string());
    }
    scopes.sort();
    scopes.dedup();
    Ok(scopes)
}

impl Store {
    /// The enrolled record for `(platform, account)`, if any.
    pub fn platform_credential(
        &self,
        platform: &str,
        account: &str,
    ) -> Result<Option<CredentialRecord>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT * FROM platform_credentials WHERE platform=?1 AND account=?2",
            params![platform, account],
            credential_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Every enrolled credential record, sorted by platform/account.
    pub fn platform_credentials(&self) -> Result<Vec<CredentialRecord>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM platform_credentials ORDER BY platform, account")?;
        let rows = stmt.query_map([], credential_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Insert the custody record for `(platform, account)` and its
    /// `platform_connected` audit event in one transaction — a record
    /// never lands without its audit. `rotated` marks the
    /// re-enrollment [`Self::platform_rotate`] performs. `risk` names
    /// the custody exposure the operator explicitly accepted
    /// (`"same-uid"` — see `custody_unprotected`); it lands on the
    /// event as `custody_risk_accepted`. Refuses a plain re-enroll:
    /// rotation is the verb that replaces bytes.
    pub fn platform_enroll(
        &self,
        record: &CredentialRecord,
        rotated: bool,
        risk: Option<&str>,
    ) -> Result<()> {
        identifier(&record.platform, "Platform")?;
        identifier(&record.account, "Account")?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT fingerprint FROM platform_credentials \
                 WHERE platform=?1 AND account=?2",
                params![record.platform, record.account],
                |r| r.get(0),
            )
            .optional()?;
        match (existing, rotated) {
            (Some(_), false) => {
                return Err(Error::rejected(format!(
                    "platform '{}' account '{}' is already enrolled — \
                     `platform rotate` replaces its credential",
                    record.platform, record.account
                )))
            }
            // A revoke landing between the caller's check and this
            // transaction must not resurrect the account — rotate into
            // a missing record refuses; enroll is the verb.
            (None, true) => {
                return Err(Error::rejected(format!(
                    "no credential is enrolled for {}/{} — `platform enroll` it first",
                    record.platform, record.account
                )))
            }
            (Some(old), true) => {
                Self::event(
                    &tx,
                    PLATFORM_STREAM,
                    CREDENTIAL_REVOKED_EVENT,
                    json!({"platform": record.platform, "account": record.account,
                           "fingerprint": old, "reason": "rotated",
                           "by": record.by}),
                )?;
            }
            (None, _) => {}
        }
        tx.execute(
            "INSERT OR REPLACE INTO platform_credentials
             (platform, account, scopes, fingerprint, custody, exchange, enrolled_at, by)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                record.platform,
                record.account,
                scopes_json(&record.scopes)?,
                record.fingerprint,
                record.custody,
                record.exchange,
                record.enrolled_at,
                record.by,
            ],
        )?;
        let mut payload = record.to_json();
        if rotated {
            payload["rotated"] = json!(true);
        }
        if let Some(risk) = risk {
            payload["custody_risk_accepted"] = json!(risk);
        }
        Self::event(&tx, PLATFORM_STREAM, PLATFORM_CONNECTED_EVENT, payload)?;
        tx.commit()?;
        Ok(())
    }

    /// Revoke `(platform, account)`'s credential: custody row gone,
    /// every grant bound to the account revoked (a grant outliving
    /// its credential silently reactivates on re-enroll — the closed
    /// default), and the §5.5 events in the same transaction. Answers
    /// the record and the revoked grants for the caller to report and
    /// for the pending-effects hook to close against; `None` when no
    /// record exists.
    pub fn platform_revoke(
        &self,
        platform: &str,
        account: &str,
        by: &str,
        reason: Option<&str>,
        effects_closed: &[String],
    ) -> Result<Option<(CredentialRecord, Vec<Grant>)>> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let record: Option<CredentialRecord> = tx
            .query_row(
                "SELECT * FROM platform_credentials WHERE platform=?1 AND account=?2",
                params![platform, account],
                credential_row,
            )
            .optional()?;
        let Some(record) = record else {
            return Ok(None);
        };
        let grants: Vec<Grant> = {
            let mut stmt = tx.prepare(
                "SELECT * FROM platform_grants WHERE platform=?1 AND account=?2 \
                 ORDER BY agent",
            )?;
            let rows = stmt.query_map(params![platform, account], grant_row)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };
        tx.execute(
            "DELETE FROM platform_grants WHERE platform=?1 AND account=?2",
            params![platform, account],
        )?;
        tx.execute(
            "DELETE FROM platform_credentials WHERE platform=?1 AND account=?2",
            params![platform, account],
        )?;
        // A revoked default stops resolving — the operator re-picks.
        tx.execute(
            "DELETE FROM platform_defaults WHERE platform=?1 AND account=?2",
            params![platform, account],
        )?;
        for grant in &grants {
            let mut payload = grant.to_json();
            payload["reason"] = json!(reason.unwrap_or("credential revoked"));
            Self::event(&tx, PLATFORM_STREAM, SCOPE_REVOKED_EVENT, payload)?;
        }
        Self::event(
            &tx,
            PLATFORM_STREAM,
            CREDENTIAL_REVOKED_EVENT,
            json!({"platform": platform, "account": account,
                   "fingerprint": record.fingerprint,
                   "reason": reason, "by": by}),
        )?;
        Self::event(
            &tx,
            PLATFORM_STREAM,
            PLATFORM_DISCONNECTED_EVENT,
            json!({"platform": platform, "account": account,
                   "fingerprint": record.fingerprint,
                   "effects_closed": effects_closed, "by": by}),
        )?;
        tx.commit()?;
        Ok(Some((record, grants)))
    }

    /// The grant `(agent, platform, account)` currently holds.
    pub fn platform_grant(
        &self,
        agent: &str,
        platform: &str,
        account: &str,
    ) -> Result<Option<Grant>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT * FROM platform_grants WHERE agent=?1 AND platform=?2 AND account=?3",
            params![agent, platform, account],
            grant_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Grants — all of them, or one agent's.
    pub fn platform_grants(&self, agent: Option<&str>) -> Result<Vec<Grant>> {
        let conn = self.conn();
        let mut out = Vec::new();
        match agent {
            Some(agent) => {
                let mut stmt = conn.prepare(
                    "SELECT * FROM platform_grants WHERE agent=?1 \
                     ORDER BY platform, account",
                )?;
                for r in stmt.query_map(params![agent], grant_row)? {
                    out.push(r?);
                }
            }
            None => {
                let mut stmt = conn
                    .prepare("SELECT * FROM platform_grants ORDER BY agent, platform, account")?;
                for r in stmt.query_map([], grant_row)? {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }

    /// Record `(agent, platform, account, scopes)` — the operator's
    /// act. An existing grant on the same triple is widened to the
    /// union of scopes; a new grant lands with its `scope_granted`
    /// event in one transaction.
    pub fn platform_grant_add(
        &self,
        agent: &str,
        platform: &str,
        account: &str,
        scopes: &[String],
        by: &str,
    ) -> Result<Grant> {
        identifier(agent, "Agent")?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        // The credential must exist — a grant on nothing is a latent
        // privilege the next enroll would silently arm. The built-in
        // `local/local` account is the exception (CAD-577): it is always
        // available with no enrollment, so a grant on it is recordable.
        let enrolled: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM platform_credentials WHERE platform=?1 AND account=?2",
                params![platform, account],
                |r| r.get(0),
            )
            .optional()?;
        if enrolled.is_none() && !crate::platform::is_builtin(platform, account) {
            return Err(Error::rejected(format!(
                "platform '{platform}' account '{account}' is not enrolled — \
                 `platform enroll` first"
            )));
        }
        let existing: Option<Grant> = tx
            .query_row(
                "SELECT * FROM platform_grants WHERE agent=?1 AND platform=?2 AND account=?3",
                params![agent, platform, account],
                grant_row,
            )
            .optional()?;
        let merged: Vec<String> = match &existing {
            Some(g) => {
                let mut all = g.scopes.clone();
                for s in scopes {
                    if !all.contains(s) {
                        all.push(s.clone());
                    }
                }
                all.sort();
                all
            }
            None => {
                let mut s = scopes.to_vec();
                s.sort();
                s
            }
        };
        let grant = Grant {
            agent: agent.to_string(),
            platform: platform.to_string(),
            account: account.to_string(),
            scopes: merged,
            granted_at: existing.as_ref().map_or(now(), |g| g.granted_at),
            by: by.to_string(),
        };
        tx.execute(
            "INSERT OR REPLACE INTO platform_grants
             (agent, platform, account, scopes, granted_at, by)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                grant.agent,
                grant.platform,
                grant.account,
                scopes_json(&grant.scopes)?,
                grant.granted_at,
                grant.by,
            ],
        )?;
        // Audit names the scopes this call added, not the merged set.
        let added: Vec<&String> = match &existing {
            Some(g) => grant
                .scopes
                .iter()
                .filter(|s| !g.scopes.contains(s))
                .collect(),
            None => grant.scopes.iter().collect(),
        };
        Self::event(
            &tx,
            PLATFORM_STREAM,
            SCOPE_GRANTED_EVENT,
            json!({"agent": agent, "platform": platform, "account": account,
                   "scopes": added, "by": by}),
        )?;
        tx.commit()?;
        Ok(grant)
    }

    /// Revoke `scopes` from `(agent, platform, account)`'s grant —
    /// `None` drops the grant whole. Answers `(existed, surviving)`:
    /// whether a grant was there at all, and the grant that remains
    /// (`None` when the revoke took it whole).
    pub fn platform_grant_revoke(
        &self,
        agent: &str,
        platform: &str,
        account: &str,
        scopes: Option<&[String]>,
        by: &str,
    ) -> Result<(bool, Option<Grant>)> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let existing: Option<Grant> = tx
            .query_row(
                "SELECT * FROM platform_grants WHERE agent=?1 AND platform=?2 AND account=?3",
                params![agent, platform, account],
                grant_row,
            )
            .optional()?;
        let Some(grant) = existing else {
            return Ok((false, None));
        };
        let (kept, revoked): (Vec<String>, Vec<String>) = match scopes {
            None => (Vec::new(), grant.scopes.clone()),
            Some(drop) => grant
                .scopes
                .iter()
                .cloned()
                .partition(|s| !drop.contains(s)),
        };
        if kept.is_empty() {
            tx.execute(
                "DELETE FROM platform_grants WHERE agent=?1 AND platform=?2 AND account=?3",
                params![agent, platform, account],
            )?;
        } else {
            tx.execute(
                "UPDATE platform_grants SET scopes=?4 \
                 WHERE agent=?1 AND platform=?2 AND account=?3",
                params![agent, platform, account, scopes_json(&kept)?],
            )?;
        }
        if !revoked.is_empty() {
            Self::event(
                &tx,
                PLATFORM_STREAM,
                SCOPE_REVOKED_EVENT,
                json!({"agent": agent, "platform": platform, "account": account,
                       "scopes": revoked, "by": by}),
            )?;
        }
        tx.commit()?;
        Ok((
            true,
            if kept.is_empty() {
                None
            } else {
                Some(Grant {
                    scopes: kept,
                    ..grant
                })
            },
        ))
    }

    /// Record `project`'s default account for `platform`. The account
    /// must be enrolled — a default that names nothing resolves to a
    /// confusing refusal downstream.
    pub fn platform_default_set(
        &self,
        project: &str,
        platform: &str,
        account: &str,
        by: &str,
    ) -> Result<ProjectDefault> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let enrolled: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM platform_credentials WHERE platform=?1 AND account=?2",
                params![platform, account],
                |r| r.get(0),
            )
            .optional()?;
        if enrolled.is_none() && !crate::platform::is_builtin(platform, account) {
            return Err(Error::rejected(format!(
                "platform '{platform}' account '{account}' is not enrolled — \
                 `platform enroll` first"
            )));
        }
        tx.execute(
            "INSERT OR REPLACE INTO platform_defaults
             (project, platform, account, set_at, by) VALUES(?1,?2,?3,?4,?5)",
            params![project, platform, account, now(), by],
        )?;
        Self::event(
            &tx,
            PLATFORM_STREAM,
            PLATFORM_DEFAULT_EVENT,
            json!({"project": project, "platform": platform,
                   "account": account, "by": by}),
        )?;
        tx.commit()?;
        Ok(ProjectDefault {
            project: project.to_string(),
            platform: platform.to_string(),
            account: account.to_string(),
            set_at: now(),
            by: by.to_string(),
        })
    }

    /// `project`'s default account for `platform`, if one is set.
    pub fn platform_default(
        &self,
        project: &str,
        platform: &str,
    ) -> Result<Option<ProjectDefault>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT * FROM platform_defaults WHERE project=?1 AND platform=?2",
            params![project, platform],
            |row| {
                Ok(ProjectDefault {
                    project: row.get("project")?,
                    platform: row.get("platform")?,
                    account: row.get("account")?,
                    set_at: row.get("set_at")?,
                    by: row.get("by")?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Every project default — `platform_defaults` lists them.
    pub fn platform_defaults(&self) -> Result<Vec<ProjectDefault>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM platform_defaults ORDER BY project, platform")?;
        let rows = stmt.query_map([], |row| {
            Ok(ProjectDefault {
                project: row.get("project")?,
                platform: row.get("platform")?,
                account: row.get("account")?,
                set_at: row.get("set_at")?,
                by: row.get("by")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---------- CAD-577: app-derived grants ----------

    /// Record the grants an app's approval derives: for each `(agent,
    /// platform, account, scopes)` the app's workflow steps declare on
    /// their bound slots, add a grant row and an `app_granted` audit
    /// event in one transaction. The grants are the app's own —
    /// `app_grants` records which app derived each, so revoking the
    /// app's approval revokes exactly these and never a hand-made
    /// grant. A re-approval re-derives: an existing app grant is
    /// replaced with the current scope set.
    pub fn app_grants_set(
        &self,
        app: &str,
        grants: &[(String, String, String, Vec<String>)],
        by: &str,
    ) -> Result<()> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        // The app's own prior grants go first — a re-approval
        // re-derives, so a scope the new structure dropped is gone.
        let prior: Vec<(String, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT agent, platform, account FROM app_grants WHERE app=?1",
            )?;
            let rows = stmt.query_map(params![app], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.flatten().collect()
        };
        for (agent, platform, account) in &prior {
            tx.execute(
                "DELETE FROM app_grants WHERE app=?1 AND agent=?2 AND platform=?3 \
                 AND account=?4",
                params![app, agent, platform, account],
            )?;
        }
        tx.execute("DELETE FROM app_grants WHERE app=?1", params![app])?;
        let mut granted = 0usize;
        for (agent, platform, account, scopes) in grants {
            identifier(agent, "Agent")?;
            let mut scopes = scopes.clone();
            scopes.sort();
            scopes.dedup();
            if scopes.is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR REPLACE INTO app_grants
                 (app, agent, platform, account, scopes, granted_at, by)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![app, agent, platform, account, scopes_json(&scopes)?, now(), by],
            )?;
            // The platform grant is the UNION of the app's derived
            // scopes and whatever the agent already held — a hand-made
            // grant's other scopes survive the derivation (CAD-577).
            let existing: Option<Grant> = tx
                .query_row(
                    "SELECT * FROM platform_grants WHERE agent=?1 AND platform=?2 \
                     AND account=?3",
                    params![agent, platform, account],
                    grant_row,
                )
                .optional()?;
            let merged: Vec<String> = match &existing {
                Some(g) => {
                    let mut all = g.scopes.clone();
                    for s in &scopes {
                        if !all.contains(s) {
                            all.push(s.clone());
                        }
                    }
                    all.sort();
                    all
                }
                None => scopes.clone(),
            };
            tx.execute(
                "INSERT OR REPLACE INTO platform_grants
                 (agent, platform, account, scopes, granted_at, by)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![agent, platform, account, scopes_json(&merged)?, now(), by],
            )?;
            Self::event(
                &tx,
                PLATFORM_STREAM,
                APP_GRANTED_EVENT,
                json!({"app": app, "agent": agent, "platform": platform,
                       "account": account, "scopes": scopes, "by": by}),
            )?;
            granted += 1;
        }
        if granted == 0 {
            // Nothing to grant — still record the derivation so the
            // audit shows the app was approved with no scopes.
            Self::event(
                &tx,
                PLATFORM_STREAM,
                APP_GRANTED_EVENT,
                json!({"app": app, "agents": 0, "by": by}),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Revoke every grant an app's approval derived: the `app_grants`
    /// rows go, and each `(agent, platform, account)`'s `platform_grants`
    /// scopes shrink by exactly the app's derived set — a hand-made
    /// grant's other scopes survive. Answers the `(agent, platform,
    /// account)` triples whose platform grant changed, so the caller
    /// can drain the waiting effects that lost a scope (CAD-506).
    pub fn app_grants_revoke(
        &self,
        app: &str,
        by: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let rows: Vec<(String, String, String, Vec<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT agent, platform, account, scopes FROM app_grants WHERE app=?1",
            )?;
            let rows = stmt.query_map(params![app], |r| {
                let scopes: String = r.get(3)?;
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, scopes_of(&scopes)))
            })?;
            rows.flatten().collect()
        };
        tx.execute("DELETE FROM app_grants WHERE app=?1", params![app])?;
        let mut changed = Vec::new();
        for (agent, platform, account, derived) in &rows {
            let existing: Option<Grant> = tx
                .query_row(
                    "SELECT * FROM platform_grants WHERE agent=?1 AND platform=?2 \
                     AND account=?3",
                    params![agent, platform, account],
                    grant_row,
                )
                .optional()?;
            let Some(existing) = existing else {
                continue;
            };
            let kept: Vec<String> = existing
                .scopes
                .iter()
                .filter(|s| !derived.contains(s))
                .cloned()
                .collect();
            if kept.is_empty() {
                tx.execute(
                    "DELETE FROM platform_grants WHERE agent=?1 AND platform=?2 \
                     AND account=?3",
                    params![agent, platform, account],
                )?;
            } else {
                tx.execute(
                    "UPDATE platform_grants SET scopes=?4 WHERE agent=?1 AND \
                     platform=?2 AND account=?3",
                    params![agent, platform, account, scopes_json(&kept)?],
                )?;
            }
            Self::event(
                &tx,
                PLATFORM_STREAM,
                APP_GRANTS_REVOKED_EVENT,
                json!({"app": app, "agent": agent, "platform": platform,
                       "account": account, "scopes": derived, "by": by}),
            )?;
            changed.push((agent.clone(), platform.clone(), account.clone()));
        }
        tx.commit()?;
        Ok(changed)
    }
}
