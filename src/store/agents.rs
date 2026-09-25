//! Agent rows: registration, identity, quota, state and GC.

use crate::adapter::registry;
use crate::error::{Error, Result};
use crate::proto::identifier;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

use super::messages::{row_message, Message};
use super::quota::{canonical_quota, merge_quota_json, quota_now_iso};
use super::schema::AdoptEntry;
use super::{now, Store};

/// The agent-state vocabulary — the `agents.state` column values —
/// for `agent list --state` (CAD-437). `attention` is a fence, `error`
/// is recorded on the row but never stored as a state.
pub const AGENT_STATES: &[&str] = &[
    "starting",
    "idle",
    "busy",
    "waiting_input",
    "attention",
    "stopping",
    "stopped",
    "offline",
];

#[derive(Debug, Clone)]
pub struct Agent {
    pub alias: String,
    pub provider: String,
    pub endpoint_kind: String,
    pub role: String,
    /// Model-preference role. Independent of runtime `role`.
    pub team_role: Option<String>,
    pub cwd: String,
    pub sandbox: String,
    pub instructions: Option<String>,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    /// Provider-confirmed reasoning effort from the most recent open.
    /// Requested/configured effort remains in `params`.
    pub effort: Option<String>,
    pub pid: Option<i64>,
    /// `/proc/<pid>/stat` field 22 of `pid`, read when the pid was
    /// recorded (CAD-385): with the pid it names ONE process, so a
    /// reused pid never inherits this row's alias. `None` on a row
    /// recorded before schema v14 or when `/proc` was unreadable —
    /// such a row fails closed ([`crate::peer::PidProof::Unproven`]).
    pub pid_start: Option<i64>,
    pub endpoint: Option<String>,
    /// Endpoint-specific registration options (`{"session": …}` for pty).
    pub params: Option<Value>,
    /// How `params.model` was chosen. Null on endpoints that cannot
    /// accept a model; legacy rows derive a label at read time.
    pub model_selection: Option<Value>,
    /// Provider-owned allowance telemetry. This is deliberately separate
    /// from `params`, which callers may edit for endpoint options.
    pub quota: Option<Value>,
    /// Minted by the owning adapter on every `open`; submission tokens
    /// embed it so reports from a previous endpoint generation fail.
    pub generation: Option<String>,
    pub state: String,
    pub enabled: bool,
    pub error: Option<String>,
    /// Row timestamps — `updated` is the last state write, which
    /// `session end` measures idleness from.
    pub created: f64,
    pub updated: f64,
}

/// Parameters for [`Store::register_agent`].
pub struct NewAgent<'a> {
    pub alias: &'a str,
    pub provider: &'a str,
    pub endpoint_kind: &'a str,
    pub role: &'a str,
    pub cwd: &'a str,
    pub sandbox: &'a str,
    pub instructions: Option<&'a str>,
    /// Endpoint-specific options as a JSON object (`{"session": "…"}`).
    pub params: Option<&'a str>,
    /// Model-preference role. `None` looks up the runtime role.
    pub team_role: Option<&'a str>,
    /// `inherit` (default) or `provider_default`.
    pub model_policy: Option<&'a str>,
}

/// The committed model-defaults document and its revision.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDefaultsSnapshot {
    pub revision: i64,
    pub config: crate::model_defaults::ModelDefaults,
}

fn row_agent(row: &rusqlite::Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        alias: row.get("alias")?,
        provider: row.get("provider")?,
        endpoint_kind: row.get("endpoint_kind")?,
        role: row.get("role")?,
        team_role: row.get("team_role")?,
        cwd: row.get("cwd")?,
        sandbox: row.get("sandbox")?,
        instructions: row.get("instructions")?,
        thread_id: row.get("thread_id")?,
        session_id: row.get("session_id")?,
        model: row.get("model")?,
        effort: row.get("effort")?,
        pid: row.get("pid")?,
        pid_start: row.get("pid_start")?,
        endpoint: row.get("endpoint")?,
        params: row
            .get::<_, Option<String>>("params")?
            .and_then(|p| serde_json::from_str(&p).ok()),
        model_selection: row
            .get::<_, Option<String>>("model_selection")?
            .and_then(|p| serde_json::from_str(&p).ok()),
        quota: row
            .get::<_, Option<String>>("quota")?
            .and_then(|q| serde_json::from_str(&q).ok()),
        generation: row.get("generation")?,
        state: row.get("state")?,
        enabled: row.get::<_, i64>("enabled")? != 0,
        error: row.get("error")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

impl Agent {
    fn param_str(&self, key: &str) -> Option<&str> {
        self.params.as_ref()?.get(key)?.as_str()
    }

    fn presented_selection(&self) -> Value {
        if let Some(selection) = &self.model_selection {
            return selection.clone();
        }
        crate::model_defaults::legacy_selection(
            registry::supports_model(&self.provider, &self.endpoint_kind),
            self.team_role.as_deref(),
            &self.role,
            self.param_str("model"),
        )
    }

    pub fn to_json(&self) -> Value {
        // Codex's effective approval policy and whether an operator chose
        // it — an absent key is the cadence default, not a decision.
        let (approval_policy, approval_policy_source) = if self.provider == "codex" {
            match self.param_str("approval_policy") {
                Some(policy) => (Some(policy), Some("configured")),
                None => (
                    Some(registry::CODEX_DEFAULT_APPROVAL_POLICY),
                    Some("cadence default"),
                ),
            }
        } else {
            (None, None)
        };
        json!({
            "alias": self.alias, "provider": self.provider,
            "endpoint_kind": self.endpoint_kind, "role": self.role,
            "team_role": self.team_role,
            "cwd": self.cwd, "sandbox": self.sandbox,
            "thread_id": self.thread_id, "session_id": self.session_id,
            "model": self.model, "pid": self.pid, "pid_start": self.pid_start,
            "state": self.state,
            // What the endpoint runs vs what it was told: the reported
            // model beside the configured launch params, with an
            // unconfigured model named as the provider's default.
            "model_reported": self.model,
            "model_effective": self.model,
            "model_configured": self.param_str("model"),
            "model_source": if self.param_str("model").is_some() {
                "configured"
            } else {
                "provider default"
            },
            "model_lookup_role": self.presented_selection().get("lookup_role").cloned().unwrap_or(Value::Null),
            "model_selection": self.presented_selection(),
            "effort": self.param_str("effort"),
            "effort_configured": self.param_str("effort"),
            "effort_reported": self.effort,
            "effort_effective": self.effort,
            "effort_source": if self.effort.is_some() {
                "provider reported"
            } else if self.param_str("effort").is_some() {
                "unknown"
            } else {
                "provider default"
            },
            "approval_policy": approval_policy,
            "approval_policy_source": approval_policy_source,
            "enabled": self.enabled, "error": self.error,
            "endpoint": self.endpoint, "params": self.params,
            "quota": self.quota,
            "generation": self.generation,
            // Last state write — `session end` measures idleness from it.
            "updated": self.updated,
            // Dead = no live endpoint address: the row cannot be
            // attached or submitted to until it opens again.
            "dead": self.endpoint.is_none(),
        })
    }
}

impl Store {
    pub fn agent(&self, alias: &str) -> Result<Agent> {
        self.agent_opt(alias)?
            .ok_or_else(|| Error::rejected("Unknown managed agent"))
    }

    pub fn agent_opt(&self, alias: &str) -> Result<Option<Agent>> {
        let conn = self.conn();
        match conn.query_row("SELECT * FROM agents WHERE alias=?", [alias], row_agent) {
            Ok(agent) => Ok(Some(agent)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    /// Look an agent up by a provider-native identifier — `thread_id` or
    /// `session_id` (e.g. a Devin session slug). Exact aliases always win;
    /// callers should try [`Store::agent_opt`] first.
    pub fn agent_by_native(&self, native: &str) -> Result<Option<Agent>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM agents WHERE thread_id=?1 OR session_id=?1 LIMIT 2")?;
        let rows = stmt
            .query_map([native], row_agent)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.len() > 1 {
            return Err(Error::rejected(
                "Native session id matches more than one agent — use the alias",
            ));
        }
        Ok(rows.into_iter().next())
    }

    pub fn agents(&self) -> Result<Vec<Agent>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM agents ORDER BY alias")?;
        let rows = stmt.query_map([], row_agent)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Register an agent. `endpoint_kind` is the delivery mechanism —
    /// `managed`/`managed-ws`/`pty` spawn actors; `inbox` is a durable
    /// mailbox row with no actor; `fake` is the test double.
    pub fn register_agent(&self, new: &NewAgent) -> Result<()> {
        identifier(new.alias, "Agent alias")?;
        identifier(new.provider, "Provider")?;
        identifier(new.endpoint_kind, "Endpoint kind")?;
        if !matches!(new.role, "pm" | "worker") {
            return Err(Error::rejected("Role must be pm or worker"));
        }
        if !matches!(new.sandbox, "read-only" | "workspace-write") {
            return Err(Error::rejected(
                "Sandbox must be read-only or workspace-write",
            ));
        }
        if !Path::new(new.cwd).is_dir() {
            return Err(Error::rejected("Working directory must be a directory"));
        }
        if let Some(text) = new.instructions {
            if text.len() > 32_000 {
                return Err(Error::rejected(
                    "Instructions must be at most 32000 characters",
                ));
            }
        }
        if let Some(p) = new.params {
            let parsed: Value = serde_json::from_str(p)
                .map_err(|_| Error::rejected("params must be a JSON object"))?;
            if !parsed.is_object() || p.len() > 4_000 {
                return Err(Error::rejected(
                    "params must be a JSON object of at most 4000 characters",
                ));
            }
        }
        let mut conn = self.conn();
        // IMMEDIATE so a concurrent register waits, then observes the
        // committed alias, instead of resolving against a stale snapshot
        // and returning `params_too_large`.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // A relaunch of a saved alias must keep that row. Resolving the
        // current defaults first can reject a previously valid near-cap
        // params blob with `params_too_large` and hide the duplicate.
        if Self::agent_alias_exists(&tx, new.alias)? {
            return Err(Self::duplicate_alias());
        }
        let defaults = Self::read_model_defaults_tx(&tx)?;
        let resolved = match crate::model_defaults::resolve(crate::model_defaults::ResolveRequest {
            provider: new.provider,
            endpoint_kind: new.endpoint_kind,
            runtime_role: new.role,
            team_role: new.team_role,
            model_policy: new.model_policy,
            params: new.params,
            config: &defaults.config,
            revision: defaults.revision,
        }) {
            Ok(resolved) => resolved,
            Err(err) => {
                if Self::agent_alias_exists(&tx, new.alias)? {
                    return Err(Self::duplicate_alias());
                }
                return Err(err);
            }
        };
        let selection = resolved.model_selection.as_ref().map(Value::to_string);
        let merged_params = match resolved.params.as_deref() {
            Some(raw) => serde_json::from_str(raw)?,
            None => json!({}),
        };
        if let Err(err) =
            registry::validate_launch_params(new.provider, new.endpoint_kind, &merged_params)
        {
            if Self::agent_alias_exists(&tx, new.alias)? {
                return Err(Self::duplicate_alias());
            }
            return Err(err);
        }
        // Inbox agents are durable mailboxes, not processes: they
        // register directly into `idle` with a stable pseudo-endpoint
        // (so `dead` reads false) and never spawn an actor.
        let (state, endpoint) = if !registry::has_actor(new.provider, new.endpoint_kind) {
            ("idle", Some(format!("inbox://{}", new.alias)))
        } else {
            ("starting", None)
        };
        tx.execute(
            "INSERT INTO agents(alias,provider,endpoint_kind,role,team_role,cwd,sandbox,
                               instructions,params,model_selection,state,endpoint,created,updated)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                new.alias,
                new.provider,
                new.endpoint_kind,
                new.role,
                resolved.team_role,
                new.cwd,
                new.sandbox,
                new.instructions,
                resolved.params,
                selection,
                state,
                endpoint,
                now(),
                now()
            ],
        )?;
        Self::event(
            &tx,
            new.alias,
            "registered",
            json!({"provider": new.provider,
                   "endpoint_kind": new.endpoint_kind, "role": new.role,
                   "team_role": resolved.team_role}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Same text `INSERT` raises for `agents.alias`, including the `sqlite:`
    /// prefix added when a rusqlite constraint error becomes [`Error`].
    /// Launch reuses a saved agent when the message contains `UNIQUE`.
    fn duplicate_alias() -> Error {
        Error::Internal("sqlite: UNIQUE constraint failed: agents.alias".to_string())
    }

    fn agent_alias_exists(tx: &Connection, alias: &str) -> Result<bool> {
        match tx.query_row("SELECT 1 FROM agents WHERE alias=?", [alias], |_| Ok(1i32)) {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    fn agent_in(&self, conn: &Connection, alias: &str) -> Result<Agent> {
        conn.query_row("SELECT * FROM agents WHERE alias=?", [alias], row_agent)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::rejected("Unknown managed agent"),
                other => other.into(),
            })
    }

    /// The durable binding captured when a message names a recipient. The
    /// row timestamp separates a removed/re-registered alias; the provider,
    /// endpoint and launch fields bind the trust boundary; endpoint identity
    /// fields are checked when they were known at enqueue time.
    fn agent_identity(agent: &Agent) -> Value {
        json!({
            "created": agent.created,
            // Keep the exact SQLite REAL bits alongside the human-readable
            // timestamp. JSON number round-tripping can move an epoch f64
            // by one ULP; the bits are the durable identity comparison.
            "created_bits": agent.created.to_bits(),
            "provider": agent.provider,
            "endpoint_kind": agent.endpoint_kind,
            "role": agent.role,
            "cwd": agent.cwd,
            "sandbox": agent.sandbox,
            "params": agent.params,
            "generation": agent.generation,
            "thread_id": agent.thread_id,
            "session_id": agent.session_id,
            "model": agent.model,
        })
    }

    /// Compare an enqueue-time binding with the current row. A missing
    /// enqueue-time binding is never safe. Runtime generation/session values
    /// that were unknown at enqueue remain unbound; once recorded, they must
    /// match exactly across a restart.
    fn identity_matches(expected: &Value, actual: &Agent) -> bool {
        let Some(expected) = expected.as_object() else {
            return false;
        };
        let actual = Self::agent_identity(actual);
        let stable = [
            "created_bits",
            "provider",
            "endpoint_kind",
            "role",
            "cwd",
            "sandbox",
            "params",
        ];
        if stable
            .iter()
            .any(|key| expected.get(*key) != actual.get(*key))
        {
            return false;
        }
        ["generation", "thread_id", "session_id", "model"]
            .iter()
            .all(|key| match expected.get(*key) {
                Some(value) if !value.is_null() => Some(value) == actual.get(*key),
                _ => true,
            })
    }

    /// Conditional transition: `to` applies only while the agent is in
    /// `from`, in a single UPDATE — a concurrently written `idle` /
    /// `attention` / `stopped` can never be overwritten. `error` is
    /// untouched. Returns whether the row matched.
    pub fn set_agent_state_if(&self, alias: &str, to: &str, from: &str) -> Result<bool> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE agents SET state=?,updated=? WHERE alias=? AND state=?",
            params![to, now(), alias, from],
        )?;
        Ok(n > 0)
    }

    /// Live-actor states only (`starting`, `stopping`, busy/idle
    /// transitions): the actor still owns its endpoint. Fence and
    /// terminal writes go through `set_state_detached` so the state
    /// never lands ahead of the cleared runtime fields.
    pub fn set_agent_state(&self, alias: &str, state: &str, error: Option<&str>) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE agents SET state=?,error=?,updated=? WHERE alias=?",
            params![state, error, now(), alias],
        )?;
        Ok(())
    }

    /// Persist native provider identity after a successful adapter `open`.
    /// `endpoint` is the attachable transport address (`ws://…`,
    /// `tmux://…`) when the endpoint kind exposes one; `generation`
    /// partitions submission tokens per endpoint life.
    pub fn set_identity(&self, alias: &str, id: &crate::adapter::Identity) -> Result<()> {
        self.set_identity_inner(alias, id, None, None)
    }

    /// Persist provider-owned quota telemetry atomically with a newly opened
    /// native identity. The adapter snapshot is wrapped with the authoritative
    /// store alias/provider/thread so caller-controlled fields cannot spoof it.
    pub fn set_identity_with_quota(
        &self,
        alias: &str,
        id: &crate::adapter::Identity,
        quota: Option<Value>,
    ) -> Result<()> {
        self.set_identity_inner(alias, id, None, quota.as_ref())
    }

    /// `set_identity` after a hot-restart adoption: the endpoint was
    /// re-verified against the shutdown record — the same pane, the
    /// same generation — so every recorded turn stays `running` and
    /// its token remains valid. Any *other* in-flight message for the
    /// alias still takes the generation-fence.
    pub fn set_identity_adopted(
        &self,
        alias: &str,
        id: &crate::adapter::Identity,
        entries: &[AdoptEntry],
    ) -> Result<()> {
        self.set_identity_inner(alias, id, Some(entries), None)
    }

    /// Adopted identity variant retaining the same atomic quota binding as a
    /// plain open. Managed Codex currently cannot adopt, but the API keeps the
    /// identity/quota contract explicit for adapters that can.
    pub fn set_identity_adopted_with_quota(
        &self,
        alias: &str,
        id: &crate::adapter::Identity,
        entries: &[AdoptEntry],
        quota: Option<Value>,
    ) -> Result<()> {
        self.set_identity_inner(alias, id, Some(entries), quota.as_ref())
    }

    fn set_identity_inner(
        &self,
        alias: &str,
        id: &crate::adapter::Identity,
        adopted: Option<&[AdoptEntry]>,
        quota: Option<&Value>,
    ) -> Result<()> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let agent = self.agent_in(&tx, alias)?;
        let quota = quota.map(|snapshot| {
            canonical_quota(
                alias,
                &agent.provider,
                &id.thread_id,
                snapshot,
                &quota_now_iso(),
            )
            .to_string()
        });
        // A fresh endpoint generation cannot claim reports for turns
        // submitted through the previous one — fence them as unknown.
        // Every adopted entry stays `running`; a plain open protects
        // none.
        let kept_ids: Vec<&str> = adopted
            .unwrap_or_default()
            .iter()
            .map(|e| e.message_id.as_str())
            .collect();
        let mut sql = String::from(
            "UPDATE messages SET state='unknown',
                error='endpoint restarted during in-flight submission',
                completed=? WHERE alias=? AND state='running'",
        );
        if !kept_ids.is_empty() {
            let placeholders = kept_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            sql.push_str(&format!(" AND id NOT IN ({placeholders})"));
        }
        let mut params: Vec<rusqlite::types::Value> = vec![now().into(), alias.to_string().into()];
        params.extend(kept_ids.iter().map(|k| k.to_string().into()));
        tx.execute(&sql, rusqlite::params_from_iter(params))?;
        // CAD-385: the pid is recorded WITH its process start time — on
        // every open, re-attach and hot-restart adoption alike, since all
        // of them land here — so a later process reusing the pid is told
        // apart from this endpoint. Read just after the adapter proved
        // the process its own; unreadable (a pid-less endpoint's 0, a
        // process already gone) records NULL, which fails closed.
        let pid_start = crate::peer::proc_starttime(id.pid).and_then(|s| i64::try_from(s).ok());
        tx.execute(
            "UPDATE agents SET thread_id=?,session_id=?,model=?,effort=?,pid=?,pid_start=?,
                endpoint=?,generation=?,quota=?,state='idle',updated=? WHERE alias=?",
            params![
                id.thread_id,
                id.session_id,
                id.model,
                id.effort,
                id.pid as i64,
                pid_start,
                id.endpoint,
                id.generation,
                quota,
                now(),
                alias
            ],
        )?;
        Self::event(
            &tx,
            alias,
            "ready",
            json!({"thread_id": id.thread_id, "session_id": id.session_id,
                   "model": id.model, "effort": id.effort, "pid": id.pid,
                   "endpoint": id.endpoint,
                   "generation": id.generation}),
        )?;
        for e in adopted.unwrap_or_default() {
            Self::event(
                &tx,
                alias,
                "turn_adopted",
                json!({"message": e.message_id, "turn_id": e.turn_id,
                       "pane_pid": e.pane_pid}),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Persist a provider notification only while it still belongs to the
    /// current provider/thread identity. Returns `false` for a stale or
    /// mismatched notification; such traffic must never overwrite a newer
    /// endpoint's allowance record.
    pub fn update_provider_quota(
        &self,
        alias: &str,
        provider: &str,
        expected_thread_id: &str,
        snapshot: &Value,
    ) -> Result<bool> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let agent = self.agent_in(&tx, alias)?;
        if agent.provider != provider || agent.thread_id.as_deref() != Some(expected_thread_id) {
            return Ok(false);
        }
        let mut data = agent
            .quota
            .as_ref()
            .and_then(|quota| quota.get("data"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let patch = snapshot.get("data").unwrap_or(snapshot);
        merge_quota_json(&mut data, patch);
        let merged = json!({
            "state": "reported",
            "source": snapshot
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("account/rateLimits/updated"),
            "data": data,
        });
        let observed_at = quota_now_iso();
        let canonical = canonical_quota(alias, provider, expected_thread_id, &merged, &observed_at);
        tx.execute(
            "UPDATE agents SET quota=? WHERE alias=?",
            params![canonical.to_string(), alias],
        )?;
        Self::event(
            &tx,
            alias,
            "quota_updated",
            json!({
                "provider": provider,
                "account_id": canonical["account_id"],
                "thread_id": expected_thread_id,
                "state": canonical["state"],
                "source": canonical["source"],
                "observed_at": canonical["observed_at"],
            }),
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Merge `patch` (a JSON object of string keys/values) into the
    /// agent's `params` — the endpoint-option bag (`auto_ready`,
    /// `upstream`, `session`). Existing keys not in the patch survive.
    /// Record the model the provider reports it is running (claude's
    /// stream `system/init`) — the `model` column, never a launch param.
    pub fn set_model_reported(&self, alias: &str, model: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE agents SET model=?,updated=? WHERE alias=?",
            params![model, now(), alias],
        )?;
        Ok(())
    }

    pub fn set_params(&self, alias: &str, patch: &Value) -> Result<()> {
        self.set_params_by(alias, patch, &Value::Null)
    }

    /// [`Store::set_params`] stamped with who asked (CAD-149): `audit`
    /// (`{"by", "by_kind", …}`, the daemon's derived caller) is merged
    /// into the `params_updated` event, which always names the target
    /// and each changed key with its old and new stored value (`null` =
    /// absent), written in the same transaction as the change.
    pub fn set_params_by(&self, alias: &str, patch: &Value, audit: &Value) -> Result<()> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let agent = self.agent_in(&tx, alias)?;
        let mut merged = agent.params.clone().unwrap_or_else(|| json!({}));
        let target = merged
            .as_object_mut()
            .ok_or_else(|| Error::internal("stored params are not an object"))?;
        let patch = patch
            .as_object()
            .ok_or_else(|| Error::rejected("params patch must be a JSON object"))?;
        let model_selection = if patch.contains_key("model") {
            if !registry::supports_model(&agent.provider, &agent.endpoint_kind) {
                return Err(Error::invalid(
                    "unsupported_model_setting",
                    format!(
                        "provider '{}' endpoint '{}' does not accept a model",
                        agent.provider, agent.endpoint_kind
                    ),
                ));
            }
            let lookup = agent
                .team_role
                .clone()
                .unwrap_or_else(|| agent.role.clone());
            match patch.get("model") {
                Some(Value::Null) => Some(crate::model_defaults::explicit_override_selection(
                    &lookup, None,
                )?),
                Some(Value::String(model)) => {
                    let model = crate::model_defaults::validate_model_id(model)?;
                    Some(crate::model_defaults::explicit_override_selection(
                        &lookup,
                        Some(&model),
                    )?)
                }
                Some(_) => {
                    return Err(Error::invalid(
                        "invalid_model",
                        "model must be a non-empty string",
                    ))
                }
                None => None,
            }
        } else {
            None
        };
        for (k, v) in patch {
            if k == "model" {
                match v {
                    Value::Null => {
                        target.remove(k);
                    }
                    Value::String(model) => {
                        let model = crate::model_defaults::validate_model_id(model)?;
                        target.insert(k.clone(), json!(model));
                    }
                    _ => {
                        return Err(Error::invalid(
                            "invalid_model",
                            "model must be a non-empty string",
                        ))
                    }
                }
            } else if v.is_null() {
                target.remove(k);
            } else {
                target.insert(k.clone(), v.clone());
            }
        }
        let stored_params = merged.to_string();
        if stored_params.len() > 4_000 {
            return Err(Error::invalid(
                "params_too_large",
                "params must be a JSON object of at most 4000 characters",
            ));
        }
        if let Some(selection) = &model_selection {
            tx.execute(
                "UPDATE agents SET params=?,model_selection=?,updated=? WHERE alias=?",
                params![stored_params, selection.to_string(), now(), alias],
            )?;
        } else {
            tx.execute(
                "UPDATE agents SET params=?,updated=? WHERE alias=?",
                params![stored_params, now(), alias],
            )?;
        }
        let before = agent.params.clone().unwrap_or_else(|| json!({}));
        let changes: Vec<Value> = patch
            .keys()
            .map(|k| {
                json!({"key": k,
                       "old": before.get(k).cloned().unwrap_or(Value::Null),
                       "new": merged.get(k).cloned().unwrap_or(Value::Null)})
            })
            .collect();
        let mut detail = json!({"patch": patch, "target": alias, "changes": changes});
        if let Some(extra) = audit.as_object() {
            for (k, v) in extra {
                detail[k.as_str()] = v.clone();
            }
        }
        Self::event(&tx, alias, "params_updated", detail)?;
        tx.commit()?;
        Ok(())
    }

    pub fn model_defaults(&self) -> Result<ModelDefaultsSnapshot> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let snapshot = Self::read_model_defaults_tx(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Replace the singleton document when `document` names the current
    /// revision. The audit event commits with the row or not at all.
    pub fn replace_model_defaults(
        &self,
        document: &str,
        attribution: Option<&str>,
    ) -> Result<ModelDefaultsSnapshot> {
        let write = crate::model_defaults::parse_settings_document(document)?;
        let attribution = crate::model_defaults::normalize_attribution(attribution)?;
        let stored = serde_json::to_string(&write.config)?;
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let current = Self::read_model_defaults_tx(&tx)?;
        if current.revision != write.expected_revision {
            return Err(Error::conflict(
                current.revision,
                format!(
                    "model defaults revision is {}, not {}",
                    current.revision, write.expected_revision
                ),
            ));
        }
        let next = current.revision.checked_add(1).ok_or_else(|| {
            Error::invalid("invalid_request", "model defaults revision overflowed")
        })?;
        let at = now();
        tx.execute(
            "UPDATE model_defaults SET revision=?, document=? WHERE id=1",
            params![next, stored],
        )?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            "model_defaults_updated",
            json!({
                "revision": next,
                "before": current.config,
                "after": write.config,
                "attribution": attribution.actor,
                "transport": attribution.transport,
                "at": at,
            }),
        )?;
        tx.commit()?;
        Ok(ModelDefaultsSnapshot {
            revision: next,
            config: write.config,
        })
    }

    fn read_model_defaults_tx(tx: &Connection) -> Result<ModelDefaultsSnapshot> {
        let (revision, document): (i64, String) = tx
            .query_row(
                "SELECT revision, document FROM model_defaults WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::internal("model defaults row is missing")
                }
                other => Error::from(other),
            })?;
        let mut config: crate::model_defaults::ModelDefaults = serde_json::from_str(&document)
            .map_err(|err| Error::internal(format!("stored model defaults are invalid: {err}")))?;
        crate::model_defaults::validate_config(&mut config)?;
        Ok(ModelDefaultsSnapshot { revision, config })
    }

    /// Forget every remembered native-session handle in one write.
    /// `params.session` AND `thread_id` both feed the adapter's
    /// `desired_session`, so clearing only params would keep resuming
    /// the dead id through the thread fallback. Called when a
    /// disposable-session endpoint proves its stored id can never
    /// resume; the next open mints a fresh session instead.
    pub fn clear_native_session(&self, alias: &str) -> Result<()> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let agent = self.agent_in(&tx, alias)?;
        let old = agent
            .params
            .as_ref()
            .and_then(|p| p.get("session"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let mut merged = agent.params.unwrap_or_else(|| json!({}));
        if let Some(target) = merged.as_object_mut() {
            target.remove("session");
        }
        tx.execute(
            "UPDATE agents SET params=?,thread_id=NULL,updated=? WHERE alias=?",
            params![merged.to_string(), now(), alias],
        )?;
        Self::event(
            &tx,
            alias,
            "session_cleared",
            json!({"session": old, "thread_id": agent.thread_id}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_enabled(&self, alias: &str, enabled: bool) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE agents SET enabled=?,updated=? WHERE alias=?",
            params![enabled as i64, now(), alias],
        )?;
        Ok(())
    }

    /// Publish a detached runtime state in ONE write: the state, its
    /// error and the cleared runtime fields land together so a reader
    /// can never observe a fenced (`attention`) or terminal agent that
    /// still holds a live endpoint — `dead`/`resumable` and
    /// `agent attach` derive from exactly that pair. The pid and any
    /// attachable endpoint belong to the dead process regardless, so
    /// leaving them would also point `agent attach` at a stale address.
    pub fn set_state_detached(&self, alias: &str, state: &str, error: Option<&str>) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE agents SET state=?,error=?,pid=NULL,pid_start=NULL,endpoint=NULL,
                generation=NULL,updated=? WHERE alias=?",
            params![state, error, now(), alias],
        )?;
        Ok(())
    }

    /// Explicit removal of a dead agent: the registry row plus the
    /// history no job needs drop in one transaction (see
    /// [`Store::prune_agent_history`]). A live endpoint refuses — `agent
    /// stop` first — as does any state that could still own or start a
    /// turn. Open work also refuses, naming it: a message not yet
    /// completed/failed/interrupted/cancelled, or a non-terminal task
    /// assigned to the alias (CAD-284). `force` overrides that check
    /// only, through the normal finish paths (CAD-304 S2): a
    /// queued/submitting message is cancelled like `message cancel`
    /// (a `cancelled` notice to its `reply_to`), a running one finishes
    /// `interrupted` like any interrupted turn (`turn_finished`, the
    /// result routed to its `reply_to`). The removed alias's own stream
    /// is then pruned with the rest of its unscoped history, so the
    /// durable trace is the `reply_to` delivery plus one
    /// `agent_remove_forced` event on the daemon stream naming what was
    /// overridden and who was notified. An `unknown` message refuses
    /// even `force` until it is reconciled (CAD-304 S1), so removal
    /// never deletes an unknown row and never leaves one behind to fence
    /// a re-registered alias. `by` (`{"by", "by_kind"}`, the daemon's
    /// derived caller) stamps the `agent_removed` event every removal
    /// records on the daemon stream. Callers must hold the lifecycle
    /// check (the daemon rejects removal of an owned alias before
    /// reaching here). Returns the `reply_to` aliases a forced finish
    /// notified — the caller wakes them.
    pub fn remove_agent(&self, alias: &str, force: bool, by: &Value) -> Result<Vec<String>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let agent = self.agent_in(&tx, alias)?;
        // Inbox rows own no process or pane — their pseudo-endpoint is
        // permanent, so neither gate applies to them.
        if registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            if agent.endpoint.is_some() {
                return Err(Error::rejected(format!(
                    "Agent '{alias}' still has a live endpoint — \
                     run `cadence agent stop {alias}` first"
                )));
            }
            if !matches!(agent.state.as_str(), "stopped" | "attention" | "offline") {
                return Err(Error::rejected(format!(
                    "Agent '{alias}' is {} — only stopped, attention or offline \
                     agents without an endpoint can be removed",
                    agent.state
                )));
            }
        }
        let open: Vec<Message> = tx
            .prepare(
                "SELECT * FROM messages WHERE alias=? AND state NOT IN
                 ('completed','failed','interrupted','cancelled') ORDER BY seq",
            )?
            .query_map([alias], row_message)?
            .collect::<rusqlite::Result<_>>()?;
        let open_messages: Vec<(String, String)> = open
            .iter()
            .map(|m| (m.id.clone(), m.state.clone()))
            .collect();
        let open_tasks: Vec<(String, String)> = tx
            .prepare(
                "SELECT id,state FROM tasks WHERE assignee=? AND state NOT IN
                 ('verified','done','cancelled','failed') ORDER BY created",
            )?
            .query_map([alias], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut notify: Vec<String> = Vec::new();
        if !open_messages.is_empty() || !open_tasks.is_empty() {
            let named = |kind: &str, rows: &[(String, String)]| {
                rows.iter()
                    .map(|(id, state)| format!("{kind} {id} ({state})"))
                    .collect::<Vec<_>>()
            };
            let work = [named("message", &open_messages), named("task", &open_tasks)].concat();
            if !force {
                return Err(Error::rejected(format!(
                    "Agent '{alias}' still has open work: {} — finish, cancel \
                     or reassign it, or pass --force to remove anyway",
                    work.join(", ")
                )));
            }
            // An `unknown` outcome is never decided or discarded here:
            // closing it would claim an outcome nobody learned, keeping
            // it would fence a re-registered alias, deleting it would
            // lose the evidence (CAD-284/CAD-304 S1). Reconcile first.
            let unknown: Vec<&Message> = open.iter().filter(|m| m.state == "unknown").collect();
            if let Some(first) = unknown.first() {
                // `agent unfence` reconciles only fencing unknowns — an
                // unconfirmed nudge is closed by `message reconcile` alone.
                let unfence = if unknown.iter().any(|m| m.source != "nudge") {
                    format!(" or `cadence agent unfence {alias} --no-resume`")
                } else {
                    String::new()
                };
                return Err(Error::rejected(format!(
                    "--force cannot remove '{alias}' while message(s) {} are \
                     unknown — reconcile the outcome first: `cadence message \
                     reconcile {} --status interrupted|completed|failed`{unfence}",
                    unknown
                        .iter()
                        .map(|m| m.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    first.id
                )));
            }
            let error = format!("agent '{alias}' removed with --force");
            // Non-terminal tasks are unassigned — state, revision,
            // kickoff and history kept — so a later agent registered
            // under this alias never inherits them (CAD-304 S4), and the
            // job's PM hears which to reassign (review round-2 ruling).
            let mut unassigned: Vec<String> = Vec::new();
            for (task_id, _) in &open_tasks {
                let task = self.task_in(&tx, task_id)?;
                let job = self.job_in(&tx, &task.job_id)?;
                tx.execute(
                    "UPDATE tasks SET assignee=NULL,updated=? WHERE id=?",
                    params![now(), task.id],
                )?;
                Self::event_scoped(
                    &tx,
                    &job.pm_alias,
                    "task_unassigned",
                    json!({"task": task.id, "job": job.id, "state": task.state,
                           "revision": task.revision, "from": alias,
                           "reason": error, "by": by["by"]}),
                    Some(&job.id),
                    Some(&task.id),
                )?;
                let told = self.route_job_event(
                    &tx,
                    &job,
                    &task,
                    &task.state,
                    &format!("unassigned:{alias}"),
                    &format!(
                        "task {} ({}) is unassigned: its assignee '{alias}' was \
                         removed with --force. Reassign it with `cadence job \
                         dispatch {} --to <worker>`.",
                        task.id, task.state, task.id
                    ),
                )?;
                if told && !notify.contains(&job.pm_alias) {
                    notify.push(job.pm_alias.clone());
                }
                unassigned.push(task.id.clone());
            }
            for message in &open {
                let told = if message.state == "running" {
                    let result = json!({"status": "interrupted", "text": "",
                                        "error": error, "via": "agent_remove_forced",
                                        "by": by["by"]});
                    self.finish_in(&tx, message, "interrupted", &result, Some(&error))?
                } else {
                    // queued / submitting: never delivered to a live
                    // actor (the endpoint is gone), so cancelled —
                    // `message cancel`'s write, event and notice.
                    let result = json!({"status": "cancelled", "via": "agent_remove_forced",
                                        "by": by["by"], "reason": error});
                    tx.execute(
                        "UPDATE messages SET state='cancelled',result=?,error=?,completed=?
                         WHERE id=?",
                        params![result.to_string(), error, now(), message.id],
                    )?;
                    Self::event(
                        &tx,
                        alias,
                        "cancelled",
                        json!({"message": message.id, "by": by["by"], "reason": error}),
                    )?;
                    self.route_notice(&tx, message, "cancelled", &result)?
                };
                // Only recipients a notice actually reached — a
                // dedupe hit or a `handoff_unresolved` is not notified.
                if let (true, Some(reply_to)) = (told, &message.reply_to) {
                    if !notify.contains(reply_to) {
                        notify.push(reply_to.clone());
                    }
                }
            }
            Self::event(
                &tx,
                Self::DAEMON_STREAM,
                "agent_remove_forced",
                json!({"alias": alias, "messages": open_messages, "tasks": open_tasks,
                       "unassigned": unassigned, "notified": notify,
                       "by": by["by"], "by_kind": by["by_kind"]}),
            )?;
        }
        Self::prune_agent_history(&tx, alias)?;
        tx.execute("DELETE FROM agents WHERE alias=?", [alias])?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            "agent_removed",
            json!({"alias": alias, "force": force,
                   "by": by["by"], "by_kind": by["by_kind"]}),
        )?;
        tx.commit()?;
        Ok(notify)
    }

    /// Drops a removed alias's message/event history except what job
    /// history still resolves (CAD-284): messages attached to a task or
    /// named as a task's `dispatch_message` or a verdict's `message`,
    /// and job-scoped events. Those stay in place under the old alias so
    /// `job show`/`job events`/`task show` read them unchanged.
    fn prune_agent_history(tx: &Connection, alias: &str) -> Result<()> {
        tx.execute(
            "DELETE FROM messages WHERE alias=?1 AND task_id IS NULL
             AND id NOT IN (SELECT dispatch_message FROM tasks
                            WHERE dispatch_message IS NOT NULL)
             AND id NOT IN (SELECT message FROM verdicts WHERE message IS NOT NULL)",
            [alias],
        )?;
        tx.execute(
            "DELETE FROM events WHERE alias=? AND job_id IS NULL",
            [alias],
        )?;
        // CAD-319: the chat is archived, not deleted, and never inherited.
        Self::thread_detach_in(tx, alias)?;
        Ok(())
    }

    /// Agents eligible for an explicit `agent gc` sweep: dead endpoint
    /// and a terminal lifecycle state, optionally limited to rows not
    /// updated within `older_than` seconds. `agent gc` sweeps these on
    /// command; the daemon's agent-gc timer (CAD-199, off unless `[host]
    /// agent_gc_older_than_secs` is set) narrows them further through
    /// [`Store::timer_gc_remove`].
    pub fn gc_candidates(&self, older_than: Option<f64>) -> Result<Vec<Agent>> {
        let cutoff = older_than.map(|age| now() - age).unwrap_or(f64::MAX);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM agents WHERE endpoint IS NULL
             AND state IN ('attention','stopped') AND updated < ?",
        )?;
        let rows = stmt.query_map(params![cutoff], row_agent)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn agent_opt_in(&self, conn: &Connection, alias: &str) -> Result<Option<Agent>> {
        match conn.query_row("SELECT * FROM agents WHERE alias=?", [alias], row_agent) {
            Ok(a) => Ok(Some(a)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// CAD-199: the opt-in agent-gc timer's removal of one
    /// [`Store::gc_candidates`] row. Inside one transaction it re-checks
    /// everything the timer requires — endpoint NULL, state `attention`
    /// or `stopped`, not updated within `older_than` seconds, not
    /// enabled, and no message in any state but completed, failed,
    /// interrupted or cancelled (so queued, submitting, running,
    /// `unknown` and any state added later all keep the row) — then
    /// deletes the row, prunes its history as `remove_agent` does (job
    /// history stays, CAD-284) and records one
    /// `agent_gc_removed` event on the daemon stream in the same commit.
    /// `Ok(None)`: no longer eligible (resumed, messaged or touched since
    /// it was listed). Records only — frees no memory and no disk, and
    /// the removed agent can no longer be resumed.
    pub fn timer_gc_remove(&self, alias: &str, older_than: f64) -> Result<Option<Agent>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let Some(agent) = tx
            .query_row("SELECT * FROM agents WHERE alias=?", [alias], row_agent)
            .optional()?
        else {
            return Ok(None);
        };
        let at = now();
        let age = at - agent.updated;
        let open_messages: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE alias=? AND state NOT IN
             ('completed','failed','interrupted','cancelled')",
            [alias],
            |r| r.get(0),
        )?;
        let eligible = agent.endpoint.is_none()
            && matches!(agent.state.as_str(), "attention" | "stopped")
            && !agent.enabled
            && age > older_than
            && open_messages == 0;
        if !eligible {
            return Ok(None);
        }
        Self::prune_agent_history(&tx, alias)?;
        tx.execute("DELETE FROM agents WHERE alias=?", [alias])?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            "agent_gc_removed",
            json!({
                "alias": agent.alias,
                "provider": agent.provider,
                "endpoint_kind": agent.endpoint_kind,
                "state": agent.state,
                "reason": format!(
                    "agent-gc timer: no endpoint, state {}, no open or unknown \
                     messages, idle {:.0}s > older_than {:.0}s",
                    agent.state, age, older_than
                ),
                "age_secs": age.floor(),
                "older_than_secs": older_than,
                "thread_id": agent.thread_id,
                "session_id": agent.session_id,
                "records_only": true,
                "note": crate::daemon::AGENT_GC_RECORDS_ONLY,
            }),
        )?;
        tx.commit()?;
        Ok(Some(agent))
    }

    /// CAD-96: per alias, the newest durable activity and the count of
    /// messages in any state but completed, failed, interrupted or
    /// cancelled (queued, submitting, running, awaiting a report,
    /// `unknown`, and any state added later all count as busy).
    /// Activity is the newest of: a message's created/started/completed
    /// stamp, and any event on the alias's stream whose kind is not in
    /// `passive` (bookkeeping that is not delivery, report or turn work).
    /// One grouped pass over each table — the idle auto-stop timer calls
    /// this once per check, never per agent.
    pub fn auto_stop_activity(
        &self,
        passive: &[&str],
    ) -> Result<std::collections::HashMap<String, (Option<f64>, i64)>> {
        let conn = self.conn();
        let placeholders = passive.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT alias, MAX(at) FROM (
                SELECT alias, at FROM events WHERE kind NOT IN ({placeholders})
                UNION ALL
                SELECT alias, MAX(created, COALESCE(started, 0), COALESCE(completed, 0))
                  FROM messages
             ) GROUP BY alias"
        );
        let args: Vec<&dyn rusqlite::ToSql> =
            passive.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
        let mut out: std::collections::HashMap<String, (Option<f64>, i64)> =
            std::collections::HashMap::new();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(args.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))
        })?;
        for row in rows {
            let (alias, at) = row?;
            out.entry(alias).or_default().0 = at;
        }
        let mut stmt = conn.prepare(
            "SELECT alias, COUNT(*) FROM messages WHERE state NOT IN
             ('completed','failed','interrupted','cancelled') GROUP BY alias",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (alias, open) = row?;
            out.entry(alias).or_default().1 = open;
        }
        Ok(out)
    }
}
