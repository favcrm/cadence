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
//! name installations, contexts and records only. The board peer is
//! CAD-768 (`src/ui/app_records.rs`); backup/export coverage is
//! follow-up CAD-753-F2).
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_records::{
    ConsentProvenance, CsvAction, CsvDecision, CustomerProfile, RecordStore,
};

fn record_profile(params: &Value) -> Result<CustomerProfile> {
    let body = params
        .get("profile")
        .ok_or_else(|| Error::rejected("record profile is required"))?;
    CustomerProfile::parse(body)
        .map_err(|_| Error::rejected("record profile exceeds its supported shape or bounds"))
}

fn record_provenance(params: &Value) -> Result<Option<ConsentProvenance>> {
    params
        .get("consent_provenance")
        .filter(|value| !value.is_null())
        .map(ConsentProvenance::parse)
        .transpose()
}

fn record_revision(params: &Value) -> Result<i64> {
    params
        .get("expected_revision")
        .and_then(Value::as_i64)
        .filter(|revision| *revision > 0)
        .ok_or_else(|| Error::rejected("expected record revision must be a positive integer"))
}

fn record_list_scope(params: &Value) -> Result<(Option<String>, i64, Option<String>)> {
    let query = match params.get("query") {
        None => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(Error::rejected("record search query must be a string")),
    };
    let limit = match params.get("limit") {
        None => crate::store::app_records::RECORD_LIMIT,
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| i64::try_from(value).ok())
            .filter(|value| (1..=crate::store::app_records::RECORD_LIMIT).contains(value))
            .ok_or_else(|| Error::rejected("record page limit is out of bounds"))?,
        Some(_) => return Err(Error::rejected("record page limit is out of bounds")),
    };
    let cursor = match params.get("cursor") {
        None => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(Error::rejected("record cursor must be a string")),
    };
    Ok((query, limit, cursor))
}

pub(super) fn csv_text(params: &Value) -> Result<String> {
    let text = required_str(params, "csv_text")?;
    if text.is_empty() || text.len() > crate::store::app_records::CSV_TEXT_BYTES {
        return Err(Error::rejected("customer CSV exceeds its size bound"));
    }
    Ok(text.to_string())
}

pub(super) fn csv_decisions(params: &Value) -> Result<Option<Vec<CsvDecision>>> {
    let Some(list) = params.get("decisions") else {
        return Ok(None);
    };
    let list = list
        .as_array()
        .ok_or_else(|| Error::rejected("customer CSV decisions must be an array"))?;
    if list.len() > crate::store::app_records::CSV_ROWS_MAX {
        return Err(Error::rejected("customer CSV decisions exceed their bound"));
    }
    let mut decisions = Vec::with_capacity(list.len());
    for item in list {
        let fields = item
            .as_object()
            .ok_or_else(|| Error::rejected("customer CSV decision must be an object"))?;
        if fields
            .keys()
            .any(|key| !matches!(key.as_str(), "row" | "action" | "expected_revision"))
        {
            return Err(Error::rejected(
                "customer CSV decision has unsupported fields",
            ));
        }
        let row = fields
            .get("row")
            .and_then(Value::as_u64)
            .and_then(|row| i64::try_from(row).ok())
            .filter(|row| *row > 0)
            .ok_or_else(|| Error::rejected("customer CSV decision rows must be positive"))?;
        let action = match fields.get("action").and_then(Value::as_str) {
            Some("create") => CsvAction::Create,
            Some("update") => CsvAction::Update,
            Some("skip") => CsvAction::Skip,
            _ => {
                return Err(Error::rejected(
                    "customer CSV decision actions are create, update or skip",
                ))
            }
        };
        let expected_revision = match fields.get("expected_revision") {
            None => None,
            Some(Value::Number(number)) => Some(
                number
                    .as_u64()
                    .and_then(|value| i64::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .ok_or_else(|| Error::rejected("expected record revision must be positive"))?,
            ),
            Some(_) => return Err(Error::rejected("expected record revision must be positive")),
        };
        decisions.push(CsvDecision {
            row,
            action,
            expected_revision,
        });
    }
    Ok(Some(decisions))
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
            "app_record_list" => &["install_id", "context_id", "query", "limit", "cursor"],
            "app_record_show" => &["install_id", "context_id", "record_id"],
            "app_record_update" => &[
                "install_id",
                "context_id",
                "record_id",
                "expected_revision",
                "profile",
                "consent_provenance",
            ],
            "app_record_csv_preview" => &["install_id", "context_id", "csv_text"],
            "app_record_csv_import" => &[
                "install_id",
                "context_id",
                "csv_text",
                "preview_token",
                "request_id",
                "decisions",
            ],
            "app_record_csv_confirm" => &[
                "install_id",
                "context_id",
                "preview_token",
                "request_id",
                "decisions_digest",
                "csv_text",
                "decisions",
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
        let write = matches!(
            method,
            "app_record_create"
                | "app_record_update"
                | "app_record_csv_import"
                | "app_record_csv_confirm"
        );
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
            "app_record_list" => {
                let (query, limit, cursor) = record_list_scope(params)?;
                records.app_record_list_paged(context, query.as_deref(), limit, cursor.as_deref())
            }
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
                record_provenance(params)?.as_ref(),
            ),
            "app_record_csv_preview" => records.app_record_csv_preview(context, &csv_text(params)?),
            "app_record_csv_import" => records.app_record_csv_import(
                context,
                &csv_text(params)?,
                required_str(params, "preview_token")?,
                required_str(params, "request_id")?,
                csv_decisions(params)?,
                // The operator's own direct import needs no confirm —
                // the operator connection IS the authority; the
                // confirm receipt exists only for the delegated agent.
                None,
            ),
            // CAD-1014: the operator's explicit confirm of the exact
            // byte-bound plan — mints the host-held one-use nonce the
            // assistant import redeems. `decisions_digest` is the
            // material digest of the confirmed decisions array
            // (`sha256:`); the confirm binds token + scope + request +
            // decisions, never agent text.
            "app_record_csv_confirm" => records.app_record_csv_confirm(
                context,
                required_str(params, "request_id")?,
                required_str(params, "preview_token")?,
                required_str(params, "decisions_digest")?,
                &csv_text(params)?,
                params.get("decisions").unwrap_or(&Value::Null),
            ),
            _ => Err(Error::rejected("unknown app record method")),
        }?;
        if write {
            // Best-effort audit on core; the file commit above stands
            // either way. Imports audit counts once; single writes
            // audit their record.
            if method == "app_record_csv_import" {
                self.store.note_app_record_csv_import(
                    install,
                    context,
                    required_str(params, "request_id").unwrap_or(""),
                    result["summary"]["applied"].as_i64().unwrap_or(0),
                    result["summary"]["skipped"].as_i64().unwrap_or(0),
                    result["summary"]["failed"].as_i64().unwrap_or(0),
                );
            } else {
                self.store.note_app_record(
                    install,
                    context,
                    required_str(params, "record_id").unwrap_or(""),
                    result["record"]["revision"].as_i64().unwrap_or(0),
                    result["record"]["digest"].as_str().unwrap_or(""),
                    method == "app_record_create",
                );
            }
            self.wake();
        }
        Ok(result)
    }
}
