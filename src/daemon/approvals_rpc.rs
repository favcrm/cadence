//! CAD-534: `cadence daemon` approvals RPC handlers — moved verbatim from src/daemon.rs; the
//! item→file map is src/daemon/split-map.toml
//! (scripts/split-daemon regenerates it).

use super::*;

impl Shared {
    /// `rollout_grant` (CAD-384) — the operator lets `agent` claim the
    /// rollout lease, and so stop this daemon from its own pane while it
    /// holds it. Operator only, by the connection; `until_secs` bounds
    /// the grant. Recorded as a `rollout_grant` event.
    pub(super) fn rpc_rollout_grant(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("rollout grant", params, peer_pid)?;
        let agent = required_str(params, "agent")?;
        let until = optional_u64(params, "until_secs").map(|secs| epoch_secs() + secs as f64);
        crate::rollout::grant(&self.state_dir, agent, until, "operator")
    }

    /// `rollout_revoke` (CAD-384) — end `agent`'s grant. Operator only.
    pub(super) fn rpc_rollout_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("rollout revoke", params, peer_pid)?;
        crate::rollout::revoke(&self.state_dir, required_str(params, "agent")?, "operator")
    }

    /// `approval_record` — persist an operator's merge approval for one
    /// exact head as audit evidence (`id` optional: the store picks a
    /// fresh default, see `Store::record_approval`). It grants nothing: dispatch and
    /// merge never read it; `cadence audit` binds it to the landed head.
    pub(super) fn rpc_approval_record(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("approval record", params, peer_pid)?;
        let pr = params
            .get("pr")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::rejected("Missing or non-numeric 'pr'"))?;
        let approval = store::NewApproval {
            id: optional_str(params, "id"),
            source: required_str(params, "source")?,
            action: optional_str(params, "action").unwrap_or("merge"),
            head_sha: required_str(params, "head")?,
            repo: required_str(params, "repo")?,
            pr,
        };
        let (new, id) = self
            .store
            .record_approval(&approval, APPROVAL_RECORDED_VIA)?;
        Ok(json!({
            "state": "recorded",
            "duplicate": !new,
            "approval_id": id,
            "source": approval.source,
            "action": approval.action,
            "head_sha": approval.head_sha,
            "scope": {"repo": approval.repo, "pr": approval.pr},
            "recorded_via": APPROVAL_RECORDED_VIA,
        }))
    }

    /// `approval_revoke` — the only way an approval is withdrawn. A
    /// cancelled or superseded message never reaches this.
    pub(super) fn rpc_approval_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("approval revoke", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let source = required_str(params, "source")?;
        let reason = required_str(params, "reason")?;
        let new = self
            .store
            .revoke_approval(id, source, reason, APPROVAL_RECORDED_VIA)?;
        Ok(json!({
            "state": "revoked",
            "duplicate": !new,
            "approval_id": id,
            "source": source,
            "reason": reason,
            "recorded_via": APPROVAL_RECORDED_VIA,
        }))
    }
}
