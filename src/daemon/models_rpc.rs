//! CAD-534: `cadence daemon` models RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

impl Shared {
    pub(super) fn rpc_model_defaults_get(self: &Arc<Self>) -> Result<Value> {
        self.model_defaults_snapshot()
    }

    /// Model defaults pick the model every agent launches with, so the
    /// write is operator authority ([`Self::operator_connection`],
    /// CAD-337) and the audit names the connection that proved it —
    /// never a caller-supplied attribution, which is refused.
    pub(super) fn rpc_model_defaults_set(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("model defaults set", params, peer_pid)?;
        let document = match params.get("document") {
            Some(Value::String(raw)) => raw.as_str(),
            Some(_) => {
                return Err(Error::invalid(
                    "invalid_request",
                    "'document' must be a string",
                ))
            }
            None => {
                return Err(Error::invalid(
                    "invalid_request",
                    "Missing required parameter 'document'",
                ))
            }
        };
        self.store
            .replace_model_defaults(document, Some(MODEL_DEFAULTS_ATTRIBUTION))?;
        self.model_defaults_snapshot()
    }

    fn model_defaults_snapshot(self: &Arc<Self>) -> Result<Value> {
        let snapshot = self.store.model_defaults()?;
        let agents = self.store.agents()?;
        let observed: Vec<crate::model_defaults::ObservedModel> = agents
            .iter()
            .map(|agent| crate::model_defaults::ObservedModel {
                provider: agent.provider.as_str(),
                configured: agent
                    .params
                    .as_ref()
                    .and_then(|params| params.get("model"))
                    .and_then(Value::as_str),
                reported: agent.model.as_deref(),
            })
            .collect();
        Ok(crate::model_defaults::snapshot_json(
            snapshot.revision,
            &snapshot.config,
            &observed,
        ))
    }

    pub(super) fn rpc_agent_settings_get(self: &Arc<Self>) -> Result<Value> {
        let agents = self.store.agents()?;
        let pm = self.pm()?;
        crate::agent_settings::snapshot(&pm.dir, &self.store, &agents)
    }

    pub(super) fn rpc_agent_settings_set(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("agent settings set", params, peer_pid)?;
        let expected = required_str(params, "expected_revision")?;
        let config = params.get("config").ok_or_else(|| {
            Error::invalid("invalid_request", "Missing required parameter 'config'")
        })?;
        let agents = self.store.agents()?;
        let pm = self.pm()?;
        crate::agent_settings::save(&pm, &self.state_dir, &self.store, &agents, expected, config)
    }

    pub(super) fn rpc_agent_settings_catalog(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("agent settings catalog", params, peer_pid)?;
        let provider = required_str(params, "provider")?;
        let pm = self.pm()?;
        crate::agent_settings::catalog(&pm.dir, provider)
    }

    pub(super) fn rpc_agent_settings_sync(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("agent settings sync", params, peer_pid)?;
        let expected = required_str(params, "expected_revision")?;
        let profile = match params.get("profile") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => Some(id.as_str()),
            Some(_) => {
                return Err(Error::invalid(
                    "invalid_request",
                    "'profile' must be a string or null",
                ))
            }
        };
        let agents = self.store.agents()?;
        let pm = self.pm()?;
        crate::agent_settings::sync(&pm, &self.store, &agents, expected, profile)
    }

    pub(super) fn rpc_agent_settings_preview(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("agent settings preview", params, peer_pid)?;
        let expected = required_str(params, "expected_revision")?;
        let profile = required_str(params, "profile")?;
        let harness = required_str(params, "harness")?;
        crate::agent_settings::preview(&self.store, expected, profile, harness)
    }
}
