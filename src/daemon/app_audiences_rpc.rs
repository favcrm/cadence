//! Strict operator management of saved segments, exclusion lists,
//! suppressions and frozen audiences (CAD-780).
//!
//! Every method first proves the operator connection, then resolves
//! the installation through the workspace catalog snapshot — an
//! unknown or diverted installation ID never reaches a file — and
//! proves the live context on core, reads included: an unknown,
//! archived or foreign context refuses before any file opens, and
//! only then opens the installation's record file. The payload
//! grammar is exact: identity-shaped (`by`, `actor`),
//! discovery-link (`project`, `project_link`) and routing
//! (`workspace`) fields are unsupported and refused, as are the
//! URL-scoped IDs themselves when they appear in a body. Predicate
//! and base values never become SQL: the store evaluates an
//! allowlisted grammar in Rust. The board peer lives in
//! `src/ui/app_audiences.rs` under this ticket; it follows the
//! CAD-768 strict-peer contract (URL IDs are authority, exact
//! transport grammar, POST-only writes).
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_audiences::{AudienceBase, Predicate};
use crate::store::app_records::RecordStore;

fn audience_predicates(params: &Value) -> Result<Vec<Predicate>> {
    let list = params
        .get("predicates")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::rejected("audience segment predicates must be an array"))?;
    if list.is_empty() || list.len() > crate::store::app_audiences::SEGMENT_PREDICATES_MAX {
        return Err(Error::rejected(
            "audience segment predicates exceed their bound",
        ));
    }
    let mut predicates = Vec::with_capacity(list.len());
    for item in list {
        let fields = item
            .as_object()
            .ok_or_else(|| Error::rejected("audience segment predicate must be an object"))?;
        if fields
            .keys()
            .any(|key| !matches!(key.as_str(), "field" | "op" | "value"))
        {
            return Err(Error::rejected(
                "audience segment predicate has unsupported fields",
            ));
        }
        predicates.push(Predicate::parse(item).map_err(|_| {
            Error::rejected("audience segment predicate exceeds its supported grammar")
        })?);
    }
    Ok(predicates)
}

fn audience_name(params: &Value) -> Result<String> {
    let name = required_str(params, "name")?;
    if name.is_empty()
        || name.len() > crate::store::app_audiences::SEGMENT_NAME_BYTES
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err(Error::rejected("audience name exceeds its bounds"));
    }
    Ok(name.to_string())
}

fn audience_base(params: &Value) -> Result<AudienceBase> {
    let body = params
        .get("base")
        .ok_or_else(|| Error::rejected("audience base is required"))?;
    if !body.is_object() {
        return Err(Error::rejected("audience base must be an object"));
    }
    AudienceBase::parse(body)
        .map_err(|_| Error::rejected("audience base exceeds its supported grammar"))
}

fn audience_exclusion(params: &Value) -> Result<Option<String>> {
    match params.get("exclusion_list_id") {
        None => Ok(None),
        Some(Value::String(id)) => {
            crate::proto::identifier(id, "exclusion list ID")?;
            Ok(Some(id.clone()))
        }
        Some(_) => Err(Error::rejected(
            "audience exclusion list ID must be a string",
        )),
    }
}

fn audience_members(params: &Value) -> Result<Vec<String>> {
    let list = params
        .get("member_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::rejected("audience exclusion members must be an array"))?;
    if list.len() > crate::store::app_audiences::EXCLUSION_MEMBERS_MAX {
        return Err(Error::rejected(
            "audience exclusion members exceed their bound",
        ));
    }
    let mut members = Vec::with_capacity(list.len());
    for id in list {
        let id = id
            .as_str()
            .ok_or_else(|| Error::rejected("audience exclusion member must be a string"))?;
        crate::proto::identifier(id, "record ID")?;
        members.push(id.to_string());
    }
    Ok(members)
}

fn audience_expected(params: &Value) -> Result<Option<i64>> {
    match params.get("expected_revision") {
        None => Ok(None),
        Some(Value::Number(number)) => Ok(Some(
            number
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| Error::rejected("expected revision must be a positive integer"))?,
        )),
        Some(_) => Err(Error::rejected(
            "expected revision must be a positive integer",
        )),
    }
}

fn audience_max(params: &Value) -> Result<i64> {
    params
        .get("max_recipients")
        .and_then(Value::as_u64)
        .and_then(|value| i64::try_from(value).ok())
        .filter(|value| (1..=crate::store::app_audiences::AUDIENCE_MAX).contains(value))
        .ok_or_else(|| Error::rejected("audience maximum recipients is out of bounds"))
}

fn audience_reason(params: &Value) -> Result<String> {
    let reason = required_str(params, "reason")?;
    if reason.is_empty()
        || reason.len() > crate::store::app_audiences::SUPPRESSION_REASON_BYTES
        || reason.chars().any(char::is_control)
    {
        return Err(Error::rejected(
            "audience suppression reason exceeds its bounds",
        ));
    }
    Ok(reason.to_string())
}

fn audience_target(params: &Value) -> Result<(Option<String>, Option<String>)> {
    let email = match params.get("email") {
        None => None,
        Some(Value::String(address)) => Some(address.clone()),
        Some(_) => {
            return Err(Error::rejected(
                "audience suppression address must be a string",
            ))
        }
    };
    let customer = match params.get("customer_id") {
        None => None,
        Some(Value::String(id)) => {
            crate::proto::identifier(id, "record ID")?;
            Some(id.clone())
        }
        Some(_) => {
            return Err(Error::rejected(
                "audience suppression customer must be a string",
            ))
        }
    };
    Ok((email, customer))
}

impl Shared {
    pub(super) fn rpc_app_audience(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app audience management", params, peer_pid)?;
        let allowed: &[&str] = match method {
            "app_segment_save" => &[
                "install_id",
                "context_id",
                "segment_id",
                "name",
                "predicates",
                "expected_revision",
            ],
            "app_segment_show" => &["install_id", "context_id", "segment_id"],
            "app_segment_list" => &["install_id", "context_id"],
            "app_exclusion_save" => &[
                "install_id",
                "context_id",
                "list_id",
                "name",
                "member_ids",
                "expected_revision",
            ],
            "app_exclusion_show" => &["install_id", "context_id", "list_id"],
            "app_exclusion_list" => &["install_id", "context_id"],
            "app_suppression_add" => {
                &["install_id", "context_id", "email", "customer_id", "reason"]
            }
            "app_suppression_remove" => &["install_id", "context_id", "email", "customer_id"],
            "app_suppression_list" => &["install_id", "context_id"],
            "app_audience_preview" => &["install_id", "context_id", "base", "exclusion_list_id"],
            "app_audience_prepare" => &[
                "install_id",
                "context_id",
                "freeze_id",
                "base",
                "exclusion_list_id",
                "max_recipients",
            ],
            "app_audience_show" => &["install_id", "context_id", "freeze_id"],
            _ => return Err(Error::rejected("unknown app audience method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app audience payload must be an object"))?;
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Error::rejected(
                "app audience payload has unsupported fields",
            ));
        }
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation ID")?;
        let context = required_str(params, "context_id")?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        let write = matches!(
            method,
            "app_segment_save"
                | "app_exclusion_save"
                | "app_suppression_add"
                | "app_suppression_remove"
                | "app_audience_prepare"
        );
        workspace::with_runtime_snapshot(&pm, install, |_, _| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Reads prove the live context too: an unknown, archived
            // or foreign context refuses before any file opens.
            self.store.app_context_proof(install, context)?;
            Ok(())
        })?;
        let records = RecordStore::open(&self.state_dir, install)?;
        let result = match method {
            "app_segment_save" => records.app_segment_save(
                context,
                required_str(params, "segment_id")?,
                audience_expected(params)?,
                &audience_name(params)?,
                &audience_predicates(params)?,
            ),
            "app_segment_show" => {
                records.app_segment_show(context, required_str(params, "segment_id")?)
            }
            "app_segment_list" => records.app_segment_list(context),
            "app_exclusion_save" => records.app_exclusion_save(
                context,
                required_str(params, "list_id")?,
                audience_expected(params)?,
                &audience_name(params)?,
                &audience_members(params)?,
            ),
            "app_exclusion_show" => {
                records.app_exclusion_show(context, required_str(params, "list_id")?)
            }
            "app_exclusion_list" => records.app_exclusion_list(context),
            "app_suppression_add" => {
                let (email, customer) = audience_target(params)?;
                records.app_suppression_add(
                    context,
                    email.as_deref(),
                    customer.as_deref(),
                    &audience_reason(params)?,
                )
            }
            "app_suppression_remove" => {
                let (email, customer) = audience_target(params)?;
                records.app_suppression_remove(context, email.as_deref(), customer.as_deref())
            }
            "app_suppression_list" => records.app_suppression_list(context),
            "app_audience_preview" => records.app_audience_preview(
                context,
                &audience_base(params)?,
                audience_exclusion(params)?.as_deref(),
            ),
            "app_audience_prepare" => records.app_audience_prepare(
                context,
                required_str(params, "freeze_id")?,
                &audience_base(params)?,
                audience_exclusion(params)?.as_deref(),
                audience_max(params)?,
            ),
            "app_audience_show" => {
                records.app_audience_show(context, required_str(params, "freeze_id")?)
            }
            _ => Err(Error::rejected("unknown app audience method")),
        }?;
        if write {
            // Best-effort audit on core; counts and digests only.
            let digest = result
                .get("segment")
                .and_then(|segment| segment.get("digest"))
                .or_else(|| result.get("exclusion").and_then(|list| list.get("digest")))
                .or_else(|| result.get("freeze").and_then(|freeze| freeze.get("digest")))
                .and_then(Value::as_str)
                .unwrap_or("");
            self.store
                .note_app_audience(install, context, method, digest);
            self.wake();
        }
        Ok(result)
    }
}
