//! CAD-1857: agent ingress endpoint registry RPC.
//!
//! The daemon owns the store and the rollout lease, so the CLI never opens
//! the database for this feature. The workflow file is resolved here, next
//! to the PM dir, and its source digest is pinned on the endpoint row.

use super::*;
use crate::issue::workflow;
use crate::store::app_runs::artifact_digest;

impl Shared {
    pub(super) fn rpc_endpoint_add(
        self: &Arc<Self>,
        params: &Value,
        _peer_pid: u32,
    ) -> Result<Value> {
        let agent = required_str(params, "agent")?;
        let kind = required_str(params, "kind")?;
        let slug = required_str(params, "slug")?;
        let project = required_str(params, "project")?;
        let name = required_str(params, "workflow")?;
        let pm = self.pm()?;
        let text = workflow::read_for(&pm.dir, project, name)?;
        let digest = artifact_digest(text.as_bytes());
        let endpoint = self
            .store
            .endpoint_add(agent, kind, slug, project, name, &digest)?;
        self.wake();
        Ok(endpoint.to_json())
    }

    pub(super) fn rpc_endpoint_list(&self, params: &Value) -> Result<Value> {
        let agent = optional_str(params, "agent");
        let state = optional_str(params, "state");
        let rows = self.store.endpoints(agent, state)?;
        Ok(json!({
            "endpoints": rows.iter().map(|endpoint| endpoint.to_json()).collect::<Vec<_>>()
        }))
    }

    pub(super) fn rpc_endpoint_show(&self, params: &Value) -> Result<Value> {
        Ok(self.store.endpoint(required_str(params, "id")?)?.to_json())
    }

    pub(super) fn rpc_endpoint_verify(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let endpoint = self.store.endpoint_verify(required_str(params, "id")?)?;
        self.wake();
        Ok(endpoint.to_json())
    }

    pub(super) fn rpc_endpoint_set_enabled(
        self: &Arc<Self>,
        params: &Value,
        enabled: bool,
    ) -> Result<Value> {
        let endpoint = self
            .store
            .endpoint_set_enabled(required_str(params, "id")?, enabled)?;
        self.wake();
        Ok(endpoint.to_json())
    }

    pub(super) fn rpc_endpoint_remove(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let endpoint = self.store.endpoint_remove(required_str(params, "id")?)?;
        self.wake();
        Ok(endpoint.to_json())
    }
}
