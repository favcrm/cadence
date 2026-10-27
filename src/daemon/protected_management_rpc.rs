//! CAD-1085: the authenticated human management surface for protected
//! Connections.
//!
//! Management is a separate authority from credential collection and from the
//! agent operations socket. Every sensitive action crosses a live
//! authenticated owner-session proof, a boot-time management eligibility
//! recheck, and ONE one-use nonce bound to the exact action and canonical
//! parameters. The durable attempt is claimed before the mutation runs, so a
//! replay, a second tab or a lost acknowledgement can never run it twice.
//!
//! Authorizing successes are committed through the protected journal and
//! require an external checkpoint acknowledgement before success is reported.
//! Key activation/unlock/lock append their own journal record; a missing or
//! lost acknowledgement is `outcome_unknown` and fences further protected
//! admission — never a silent success and never a retried mutation.

use std::sync::Arc;

use serde_json::{json, Value};

use super::{required_str, Shared};
use crate::daemon::protected_connections::ManagementEvidence;
use crate::error::{Error, Result};
use crate::store::connection_dispatch::{set_config_in, set_manifest_in, set_policy_in, Authority};
use cadence_core::connection_dispatch::dto::{ManifestV1, OperationPolicyV1, OwnerLimitsV1};
use cadence_core::connection_dispatch::primitives::Digest;

/// The exact management actions. Nothing else is addressable.
const ACTIONS: &[&str] = &[
    "custody_initialize",
    "custody_unlock",
    "custody_lock",
    "config",
    "manifest",
    "policy",
    "grant",
    "ungrant",
    "revoke_credential",
    "native_source_lock",
    "native_source_revoke",
];

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn params_digest(action: &str, params: &Value) -> String {
    cadence_core::connection_dispatch::material_digest_for_domain(
        "cadence-protected-management-params-v1",
        &json!({"v": 1, "action": action, "params": params}),
    )
}

fn action_allowed(action: &str) -> Result<()> {
    if !ACTIONS.contains(&action) {
        return Err(Error::invalid(
            "invalid_request",
            "the management action is unsupported",
        ));
    }
    Ok(())
}

fn only_keys<'a>(
    params: &'a Value,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>> {
    let object = params.as_object().ok_or_else(|| {
        Error::invalid(
            "invalid_request",
            "the management parameters must be an object",
        )
    })?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::invalid(
            "invalid_request",
            "unsupported management parameter",
        ));
    }
    Ok(object)
}

/// The live boot-time evidence plus the durable workspace. Management never
/// borrows an agent runtime's evidence.
fn management_context(shared: &Shared) -> Result<(Authority, ManagementEvidence)> {
    let eligibility = shared.protected()?.management_evidence()?;
    let authority = shared.store.protected_authority()?;
    if authority.workspace_id != eligibility.workspace_id
        || authority.database_epoch != eligibility.database_epoch
    {
        return Err(Error::invalid(
            "placement_changed",
            "the protected workspace changed",
        ));
    }
    Ok((authority, eligibility))
}

fn session(shared: &Arc<Shared>, params: &Value) -> Result<Value> {
    only_keys(params, &["action", "params", "session"])?;
    let action = required_str(params, "action")?.to_string();
    action_allowed(&action)?;
    let inner = params.get("params").cloned().unwrap_or_else(|| json!({}));
    let proof = params
        .get("session")
        .ok_or_else(|| Error::invalid("entry_session_required", "entry session is unavailable"))?;
    let session_binding = super::credential_entry_rpc::management_session_binding(shared, proof)?;
    let (authority, eligibility) = management_context(shared)?;
    let digest = params_digest(&action, &inner);
    let (nonce, attempt_id, expires) = shared.store.issue_management_nonce(
        &action,
        &digest,
        &session_binding,
        eligibility.owner_epoch.get() as i64,
        now_ms(),
    )?;
    Ok(json!({
        "attempt_id": attempt_id,
        "nonce": nonce,
        "nonce_expires_in_ms": (expires - now_ms()).max(0),
        "owner_epoch": eligibility.owner_epoch.get(),
        "workspace_id": authority.workspace_id,
        "action": action,
    }))
}

fn status(shared: &Arc<Shared>, params: &Value) -> Result<Value> {
    only_keys(params, &["attempt_id"])?;
    let attempt_id = required_str(params, "attempt_id")?;
    let attempt = shared
        .store
        .management_attempt(attempt_id)?
        .ok_or_else(|| Error::invalid("not_found", "the attempt is unavailable"))?;
    Ok(json!({
        "attempt_id": attempt.attempt_id,
        "action": attempt.action,
        "state": attempt.state,
        "code": attempt.result_code,
        "expires_at_ms": attempt.expires_at_ms,
    }))
}

/// Execute ONE claimed, bound management action. The mutation runs only after
/// the claim, and success is reported only after its checkpoint.
fn action(shared: &Arc<Shared>, params: &Value) -> Result<Value> {
    only_keys(
        params,
        &["attempt_id", "nonce", "session", "action", "params"],
    )?;
    let attempt_id = required_str(params, "attempt_id")?.to_string();
    let nonce = required_str(params, "nonce")?.to_string();
    let action_name = required_str(params, "action")?.to_string();
    action_allowed(&action_name)?;
    let inner = params.get("params").cloned().unwrap_or_else(|| json!({}));
    let proof = params
        .get("session")
        .ok_or_else(|| Error::invalid("entry_session_required", "entry session is unavailable"))?;
    let session_binding = super::credential_entry_rpc::management_session_binding(shared, proof)?;
    let (authority, eligibility) = management_context(shared)?;
    let digest = params_digest(&action_name, &inner);
    let now = now_ms();
    let claimed_attempt_id = shared.store.claim_management_attempt(
        &nonce,
        &attempt_id,
        &action_name,
        &digest,
        &session_binding,
        eligibility.owner_epoch.get() as i64,
        now,
    )?;
    let result = execute(shared, &action_name, &inner, &authority, &eligibility);
    let (applied, unknown, code) = match &result {
        Ok(_) => (true, false, None),
        Err(Error::OutcomeUnknown(_)) => (false, true, Some("outcome_unknown")),
        Err(error) if error.code() == Some("outcome_unknown") => {
            (false, true, Some("outcome_unknown"))
        }
        Err(error) => (false, false, Some(error.code().unwrap_or("refused"))),
    };
    if let Err(error) = shared.store.finish_management_attempt(
        &claimed_attempt_id,
        applied,
        unknown,
        code,
        now_ms(),
    ) {
        return Err(Error::OutcomeUnknown(format!(
            "the management outcome is unknown: {error}"
        )));
    }
    result
}

fn execute(
    shared: &Arc<Shared>,
    action: &str,
    params: &Value,
    authority: &Authority,
    eligibility: &ManagementEvidence,
) -> Result<Value> {
    let workspace = authority.workspace_id.clone();
    match action {
        "custody_initialize" | "custody_unlock" | "custody_lock" => {
            only_keys(params, &[])?;
            let operation = shared.custody_operation()?;
            let vault = shared.platform_custody.encrypted().ok_or_else(|| {
                Error::invalid(
                    "custody_unavailable",
                    "this daemon has no protected encrypted custody profile",
                )
            })?;
            match action {
                "custody_initialize" => {
                    let references: Vec<_> = shared
                        .store
                        .platform_credentials()?
                        .into_iter()
                        .filter(|record| {
                            record
                                .custody
                                .starts_with(crate::platform::encrypted_sqlite::TAG_PREFIX)
                        })
                        .collect();
                    if !references.is_empty() {
                        return Err(Error::invalid(
                            "state_conflict",
                            "encrypted core references already exist",
                        ));
                    }
                    // Metadata-only bootstrap: the vault reads and validates
                    // the operator-provisioned key file, writes the key
                    // confirmation header, and relocks. No credential is
                    // decrypted and no readiness is claimed.
                    vault.initialize(&workspace, false)?;
                    vault.lock()?;
                }
                "custody_unlock" => vault.unlock(&workspace)?,
                "custody_lock" => vault.lock()?,
                _ => unreachable!(),
            }
            let status = vault.status();
            drop(operation);
            let digest = Digest::try_from(
                cadence_core::connection_dispatch::material_digest_for_domain(
                    "cadence-protected-management-v1",
                    &json!({
                        "action": action,
                        "workspace_id": workspace,
                        "owner_epoch": eligibility.owner_epoch.get(),
                        "profile_digest": eligibility.profile_digest.as_str(),
                    }),
                ),
            )
            .map_err(|_| Error::internal("the management digest is malformed"))?;
            shared.store.protected_record_management(
                &workspace,
                eligibility.owner_epoch,
                digest,
            )?;
            Ok(json!({"action": action, "custody": status}))
        }
        "config" => {
            let object = only_keys(params, &["enabled", "limits"])?;
            let enabled = object
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::invalid("invalid_request", "'enabled' must be a boolean"))?;
            let limits: OwnerLimitsV1 = serde_json::from_value(
                object
                    .get("limits")
                    .cloned()
                    .ok_or_else(|| Error::invalid("invalid_request", "the limits are missing"))?,
            )
            .map_err(|_| Error::invalid("invalid_request", "the limits are not finite"))?;
            shared.store.protected_management_commit(
                &workspace,
                eligibility.owner_epoch,
                management_binding("config", &workspace)?,
                |tx| set_config_in(tx, enabled, &limits),
            )?;
            Ok(json!({"action": "config", "enabled": enabled}))
        }
        "manifest" => {
            let object = only_keys(params, &["manifest"])?;
            let manifest: ManifestV1 = serde_json::from_value(
                object
                    .get("manifest")
                    .cloned()
                    .ok_or_else(|| Error::invalid("invalid_request", "the manifest is missing"))?,
            )
            .map_err(|_| Error::invalid("invalid_request", "the manifest is malformed"))?;
            cadence_core::connection_dispatch::dto::validate_manifest_v1(&manifest)
                .map_err(|_| Error::invalid("invalid_request", "the manifest is malformed"))?;
            shared.store.protected_management_commit(
                &workspace,
                eligibility.owner_epoch,
                management_binding("manifest", &workspace)?,
                |tx| set_manifest_in(tx, &manifest),
            )?;
            Ok(
                json!({"action": "manifest", "manifest_id": manifest.manifest_id, "revision": manifest.revision}),
            )
        }
        "policy" => {
            let object = only_keys(
                params,
                &[
                    "connection_id",
                    "credential_revision",
                    "policy",
                    "policy_digest",
                ],
            )?;
            let connection_id = required_str(params, "connection_id")?.to_string();
            let credential_revision = object
                .get("credential_revision")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    Error::invalid("invalid_request", "the credential revision is invalid")
                })?;
            let policy: OperationPolicyV1 = serde_json::from_value(
                object
                    .get("policy")
                    .cloned()
                    .ok_or_else(|| Error::invalid("invalid_request", "the policy is missing"))?,
            )
            .map_err(|_| Error::invalid("invalid_request", "the policy is malformed"))?;
            let policy_digest = required_str(params, "policy_digest")?.to_string();
            if policy_digest != policy.approved_document_digest.as_str() {
                return Err(Error::invalid(
                    "invalid_request",
                    "the policy digest does not match the approved document",
                ));
            }
            shared.store.protected_management_commit(
                &workspace,
                eligibility.owner_epoch,
                management_binding("policy", &workspace)?,
                |tx| {
                    set_policy_in(
                        tx,
                        &connection_id,
                        credential_revision,
                        &policy,
                        &policy_digest,
                    )
                },
            )?;
            Ok(
                json!({"action": "policy", "connection_id": connection_id, "policy_revision": policy.policy_revision}),
            )
        }
        "grant" | "ungrant" => {
            let object = only_keys(params, &["agent", "platform", "account", "scopes"])?;
            let agent = required_str(params, "agent")?.to_string();
            let platform = required_str(params, "platform")?.to_string();
            let account = required_str(params, "account")?.to_string();
            let scopes: Vec<String> = match object.get("scopes") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| {
                        item.as_str().map(str::to_owned).ok_or_else(|| {
                            Error::invalid("invalid_request", "'scopes' items must be strings")
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                Some(_) => {
                    return Err(Error::invalid(
                        "invalid_request",
                        "'scopes' must be an array",
                    ))
                }
            };
            if action == "grant" {
                shared.store.platform_grant_add(
                    &agent,
                    &platform,
                    &account,
                    &scopes,
                    "human:grant",
                )?;
            } else {
                shared.store.platform_grant_revoke(
                    &agent,
                    &platform,
                    &account,
                    if scopes.is_empty() {
                        None
                    } else {
                        Some(&scopes)
                    },
                    "human:ungrant",
                )?;
            }
            // The grant writer committed its own journalled transaction; the
            // external floor must acknowledge it before this reports success.
            shared.store.protected_management_commit(
                &workspace,
                eligibility.owner_epoch,
                management_binding(action, &workspace)?,
                |_tx| Ok(()),
            )?;
            Ok(json!({"action": action, "agent": agent, "platform": platform, "account": account}))
        }
        "revoke_credential" => {
            let object = only_keys(params, &["connection_id", "expected_credential_revision"])?;
            let connection_id = required_str(params, "connection_id")?.to_string();
            let record = shared
                .store
                .connection_credential(&connection_id)?
                .ok_or_else(|| Error::invalid("not_found", "the credential is unavailable"))?;
            if let Some(expected) = object
                .get("expected_credential_revision")
                .and_then(Value::as_u64)
            {
                if expected != record.credential_revision {
                    return Err(Error::invalid(
                        "stale_revision",
                        "the credential revision changed",
                    ));
                }
            }
            shared.store.platform_revoke_encrypted(
                &record.platform,
                &record.account,
                "human:revoke",
                &record.connection_id,
                record.credential_revision,
            )?;
            shared.store.protected_management_commit(
                &workspace,
                eligibility.owner_epoch,
                management_binding("revoke_credential", &workspace)?,
                |_tx| Ok(()),
            )?;
            Ok(json!({"action": "revoke_credential", "connection_id": connection_id}))
        }
        "native_source_lock" | "native_source_revoke" => {
            only_keys(params, &["source_id"])?;
            let source_id = required_str(params, "source_id")?;
            let state = if action == "native_source_lock" {
                "locked"
            } else {
                "revoked"
            };
            shared.native_model()?.fence_source(
                &workspace,
                source_id,
                eligibility.owner_epoch,
                state,
            )?;
            Ok(json!({"action": action, "source_id": source_id, "state": state}))
        }
        _ => Err(Error::invalid(
            "invalid_request",
            "the management action is unsupported",
        )),
    }
}

fn management_binding(action: &str, workspace: &str) -> Result<Digest> {
    Digest::try_from(
        cadence_core::connection_dispatch::material_digest_for_domain(
            "cadence-protected-management-binding-v1",
            &json!({"action": action, "workspace_id": workspace}),
        ),
    )
    .map_err(|_| Error::internal("the management digest is malformed"))
}

/// The exact daemon methods the root RPC table routes here. Every one runs the
/// proven-operator gate and a strict parameter allowlist first.
pub(crate) fn rpc_protected_management(
    shared: &Arc<Shared>,
    method: &str,
    params: &Value,
    peer_pid: u32,
) -> Result<Value> {
    shared.operator_connection("protected management", params, peer_pid)?;
    match method {
        "protected_management_session" => session(shared, params),
        "protected_management_status" => status(shared, params),
        "protected_management_action" => action(shared, params),
        _ => Err(Error::rejected("unknown protected management operation")),
    }
}
