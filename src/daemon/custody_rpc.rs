//! Scoped encrypted custody admission and native operator controls.
use super::*;
use crate::platform::encrypted_sqlite::{EncryptedVault, ExpectedIdentity, TAG_PREFIX};
use crate::store::{CredentialRecord, Store};
use std::sync::MutexGuard;

// Only these statically audited daemon use sites may request a decrypt.
#[derive(Clone, Copy)]
pub(super) enum CustodyUse {
    Connections,
    OperatorSmtp,
}

/// Non-cloneable/nonserializable: borrows the held operation guard and binds
/// the exact core snapshot, workspace and configured key, not a reusable token.
pub(crate) struct CustodyWitness<'a> {
    _guard: &'a MutexGuard<'a, ()>,
    record: CredentialRecord,
    expected: ExpectedIdentity,
    _purpose: CustodyUse,
}
impl CustodyWitness<'_> {
    pub(crate) fn record(&self) -> &CredentialRecord {
        &self.record
    }
    pub(crate) fn expected(&self) -> &ExpectedIdentity {
        &self.expected
    }
    pub(crate) fn validate(&self, store: &Store) -> Result<()> {
        let live = store.connection_credential(&self.record.connection_id)?;
        if live.as_ref().map(CredentialRecord::to_json) != Some(self.record.to_json())
            || store.connection_workspace_id()? != self.expected.workspace_id
        {
            return Err(Error::invalid(
                "stale_revision",
                "custody authority changed",
            ));
        }
        Ok(())
    }
}

pub(super) struct CustodyOperation<'a> {
    guard: MutexGuard<'a, ()>,
    shared: &'a Shared,
}
impl CustodyOperation<'_> {
    pub(super) fn witness(
        &self,
        purpose: CustodyUse,
        record: &CredentialRecord,
    ) -> Result<CustodyWitness<'_>> {
        self.shared.refuse_hosted_encrypted()?;
        let vault = self.shared.encrypted_vault()?;
        let key_id = vault
            .key_id()
            .ok_or_else(|| Error::invalid("locked", "custody is locked"))?;
        let witness = CustodyWitness {
            _guard: &self.guard,
            record: record.clone(),
            expected: ExpectedIdentity {
                workspace_id: self.shared.store.connection_workspace_id()?,
                connection_id: record.connection_id.clone(),
                provider: record.platform.clone(),
                account: record.account.clone(),
                credential_revision: record.credential_revision,
                exchange: record.exchange.clone(),
                key_id,
            },
            _purpose: purpose,
        };
        witness.validate(&self.shared.store)?;
        Ok(witness)
    }
}

impl Shared {
    pub(super) fn custody_operation(&self) -> Result<CustodyOperation<'_>> {
        let guard = if self.platform_custody.encrypted().is_some() {
            match self.platform_custody_lock.try_lock() {
                Ok(g) => g,
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(Error::busy_coded("busy", "custody operation in progress"))
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    if let Some(vault) = self.platform_custody.encrypted() {
                        vault.fence();
                    }
                    return Err(Error::invalid(
                        "outcome_unknown",
                        "custody operation is fenced",
                    ));
                }
            }
        } else {
            self.platform_custody_lock
                .lock()
                .map_err(|_| Error::invalid("storage_unavailable", "custody unavailable"))?
        };
        Ok(CustodyOperation {
            guard,
            shared: self,
        })
    }

    fn encrypted_vault(&self) -> Result<&EncryptedVault> {
        self.platform_custody
            .encrypted()
            .map(|v| v.as_ref())
            .ok_or_else(|| {
                Error::invalid(
                    "unsupported_custody_use",
                    "encrypted custody is not configured",
                )
            })
    }
    pub(super) fn refuse_hosted_encrypted(&self) -> Result<()> {
        if self.smtp_internal.is_some() {
            return Err(Error::invalid(
                "unsupported_custody_use",
                "encrypted custody unavailable on hosted SMTP",
            ));
        }
        Ok(())
    }

    pub(super) fn rpc_connection_custody(
        &self,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        // Caller proof precedes classification, key access and writes on every verb.
        self.operator_connection("connection custody management", params, peer_pid)?;
        let allowed: &[&str] = match method {
            "connection_custody_initialize" | "connection_custody_unlock" => {
                &["accept_same_uid_risk"]
            }
            "connection_custody_lock" | "connection_custody_status" => &[],
            _ => return Err(Error::rejected("unknown custody operation")),
        };
        let object = params
            .as_object()
            .ok_or_else(|| Error::rejected("custody parameters must be an object"))?;
        if object.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::rejected("unsupported custody parameter"));
        }
        if method == "connection_custody_status" && self.platform_custody.encrypted().is_none() {
            return Ok(
                json!({"custody":{"configured":false,"unlocked":false,"fenced":false,"isolation":"none","protected":false}}),
            );
        }
        let _operation = self.custody_operation()?;
        let vault = self.encrypted_vault()?;
        match method {
            "connection_custody_initialize" | "connection_custody_unlock" => {
                self.refuse_hosted_encrypted()?;
                if params.get("accept_same_uid_risk").and_then(Value::as_bool) != Some(true) {
                    return Err(Error::invalid(
                        "custody_unprotected",
                        "explicit same-UID risk acknowledgement required",
                    ));
                }
                let workspace = self.store.connection_workspace_id()?;
                let references: Vec<_> = self
                    .store
                    .platform_credentials()?
                    .into_iter()
                    .filter(|r| r.custody.starts_with(TAG_PREFIX))
                    .collect();
                if method == "connection_custody_initialize" {
                    if !references.is_empty() {
                        return Err(Error::invalid(
                            "conflict",
                            "encrypted core references already exist",
                        ));
                    }
                    vault.initialize(&workspace, !references.is_empty())?;
                    // Initialization provisions a header, never enables consumer use.
                    vault.lock()?;
                } else {
                    vault.unlock(&workspace)?;
                    let inspected = (|| -> Result<()> {
                        for record in references {
                            let witness = _operation.witness(CustodyUse::Connections, &record)?;
                            let _bytes = crate::platform::load_credential_witnessed(
                                &self.store,
                                &self.platform_custody,
                                &witness,
                            )?;
                        }
                        Ok(())
                    })();
                    if let Err(error) = inspected {
                        vault.fence();
                        return Err(error);
                    }
                }
            }
            "connection_custody_lock" => vault.lock()?,
            "connection_custody_status" => {}
            _ => unreachable!(),
        }
        Ok(json!({"custody":vault.status()}))
    }
}
