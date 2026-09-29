//! Strict operator management of per-installation record files.
//!
//! Every method first proves the operator connection, then resolves
//! the installation through the workspace catalog snapshot — an
//! unknown or diverted installation ID never reaches a file — and
//! only then opens `<state_dir>/app-records/<install_id>.sqlite3`.
//! Writes additionally prove the live context on core first; the
//! record write then commits entirely inside the installation file,
//! with no cross-file transaction. The payload grammar is exact:
//! identity-shaped (`by`, `actor`), discovery-link (`project`,
//! `project_link`) and routing (`workspace`) fields are unsupported
//! and refused. File paths and SQL never leave this handler: callers
//! name installations, contexts and records only. There is no HTTP
//! peer in this slice; no HTTP route exposes these methods (the
//! board peer is follow-up CAD-753-F1; backup/export coverage is
//! follow-up CAD-753-F2).
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_records::{CustomerProfile, RecordStore};

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
        let write = matches!(method, "app_record_create" | "app_record_update");
        // The installation snapshot and (for writes) the live context
        // proof come from core; the record file opens after, in
        // sequence — never one transaction across both stores.
        workspace::with_runtime_snapshot(&pm, install, |_, _| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if write {
                self.store.app_context_proof(install, context)?;
            }
            Ok(())
        })?;
        let records = RecordStore::open(&self.state_dir, install)?;
        let result = match method {
            "app_record_list" => records.app_record_list(context),
            "app_record_show" => {
                records.app_record_show(context, required_str(params, "record_id")?)
            }
            "app_record_create" => records.app_record_create(
                context,
                required_str(params, "record_id")?,
                &record_profile(params)?,
            ),
            "app_record_update" => records.app_record_update(
                context,
                required_str(params, "record_id")?,
                record_revision(params)?,
                &record_profile(params)?,
            ),
            _ => Err(Error::rejected("unknown app record method")),
        }?;
        if write {
            // Best-effort audit on core; the file commit above stands
            // either way.
            self.store.note_app_record(
                install,
                context,
                required_str(params, "record_id").unwrap_or(""),
                result["record"]["revision"].as_i64().unwrap_or(0),
                result["record"]["digest"].as_str().unwrap_or(""),
                method == "app_record_create",
            );
            self.wake();
        }
        Ok(result)
    }
}
