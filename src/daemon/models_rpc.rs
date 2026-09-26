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
}
