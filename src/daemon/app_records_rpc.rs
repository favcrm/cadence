//! Strict operator management of host-managed installation records.
//!
//! Every method first proves the operator connection, then resolves
//! the installation through the workspace catalog snapshot — an
//! unknown or diverted installation ID never reaches the store — and
//! only then runs the typed store action. The payload grammar is
//! exact: identity-shaped (`by`, `actor`), discovery-link
//! (`project`, `project_link`) and routing (`workspace`) fields are
//! unsupported and refused. There is no HTTP peer in this slice; no
//! HTTP route exposes these methods (the board peer is follow-up
//! CAD-753-F1).
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_records::CustomerProfile;

fn record_profile(params: &Value) -> Result<CustomerProfile> {
    let body = params
        .get("profile")
        .ok_or_else(|| Error::rejected("record profile is required"))?;
    CustomerProfile::parse(body)
        .map_err(|_| Error::rejected("record profile exceeds its supported shape or bounds"))
}

fn record_revision(params: &Value) -> Result<i64> {
    params
        .get("expected_revision")
        .and_then(Value::as_i64)
        .filter(|revision| *revision > 0)
        .ok_or_else(|| Error::rejected("expected record revision must be a positive integer"))
}

impl Shared {
    pub(super) fn rpc_app_record(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app record management", params, peer_pid)?;
        let allowed: &[&str] = match method {
            "app_record_create" => &["install_id", "context_id", "record_id", "profile"],
            "app_record_list" => &["install_id", "context_id"],
            "app_record_show" => &["install_id", "context_id", "record_id"],
            "app_record_update" => &[
                "install_id",
                "context_id",
                "record_id",
                "expected_revision",
                "profile",
            ],
            _ => return Err(Error::rejected("unknown app record method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app record payload must be an object"))?;
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Error::rejected("app record payload has unsupported fields"));
        }
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation ID")?;
        let context = required_str(params, "context_id")?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        // Writes re-check the context's live proof inside the same
        // installation snapshot, the way run creation re-checks its
        // context receipt — a context archived after the caller
        // listed it refuses the write. The store re-checks liveness
        // again in its own write transaction.
        let write = matches!(method, "app_record_create" | "app_record_update");
        let result = workspace::with_runtime_snapshot(&pm, install, |_, _| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if write {
                self.store.app_context_proof(install, context)?;
            }
            match method {
                "app_record_list" => self.store.app_record_list(install, context),
                "app_record_show" => {
                    self.store
                        .app_record_show(install, context, required_str(params, "record_id")?)
                }
                "app_record_create" => self.store.app_record_create(
                    install,
                    context,
                    required_str(params, "record_id")?,
                    &record_profile(params)?,
                ),
                "app_record_update" => self.store.app_record_update(
                    install,
                    context,
                    required_str(params, "record_id")?,
                    record_revision(params)?,
                    &record_profile(params)?,
                ),
                _ => Err(Error::rejected("unknown app record method")),
            }
        })?;
        if write {
            self.wake();
        }
        Ok(result)
    }
}
