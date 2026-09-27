//! CAD-129: `cadence test` RPC — submit, status, log, and the queue
//! summary `cadence status` prints. The runner lives in
//! [`crate::test_queue`]; these handlers only parse the request.

use std::collections::BTreeMap;

use super::*;

impl Shared {
    pub(super) fn rpc_test_submit(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let worktree = required_str(params, "worktree")?;
        let filter = optional_str(params, "filter").unwrap_or("").to_string();
        let features = optional_str(params, "features").unwrap_or("").to_string();
        let priority = optional_str(params, "priority")
            .unwrap_or("dev")
            .to_string();
        let full = params.get("full").and_then(Value::as_bool).unwrap_or(false);
        let no_cache = params
            .get("no_cache")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let rustflags = optional_str(params, "rustflags").unwrap_or("").to_string();
        if rustflags.len() > 4096 {
            return Err(Error::rejected("rustflags is longer than 4096 bytes"));
        }
        let by = optional_str(params, "by").unwrap_or("operator").to_string();
        let mut env = BTreeMap::new();
        if let Some(obj) = params.get("env").and_then(Value::as_object) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    env.insert(k.clone(), s.to_string());
                }
            }
        }
        crate::test_queue::submit(
            &self.state_dir,
            &crate::test_queue::Submit {
                worktree: std::path::PathBuf::from(worktree),
                filter,
                features,
                priority,
                full,
                no_cache,
                env,
                rustflags,
                by,
            },
        )
    }

    pub(super) fn rpc_test_status(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "id")?;
        crate::test_queue::require_job_id(id)?;
        crate::test_queue::status(&self.state_dir, id)
    }

    pub(super) fn rpc_test_log(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "id")?;
        crate::test_queue::require_job_id(id)?;
        crate::test_queue::log(&self.state_dir, id)
    }

    pub(super) fn rpc_test_queue(self: &Arc<Self>) -> Result<Value> {
        crate::test_queue::queue_view(&self.state_dir)
    }
}
