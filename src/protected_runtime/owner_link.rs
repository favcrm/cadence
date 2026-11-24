//! Authenticated supervisor link and its protected store boundary.

use std::sync::{Arc, RwLock};

use cadence_core::connection_dispatch::ledger::{CheckpointAckV1, CheckpointV1, LedgerRecordV1};
use cadence_core::connection_dispatch::primitives::{Counter, Digest, Revision};

use crate::error::{Error, Result};
use crate::store::connection_dispatch::{BootAuthority, ExternalFloorV1, ProtectedCheckpointContext, ProtectedCheckpointWriter};
use crate::store::Store;

use super::client::PrestartEligibility;
use super::{LocalProfile, OwnerEvidence, QualifiedRuntime, RootOwnerClient, RuntimeStatus};

/// A supervisor link whose profile and process generation are locally verified.
/// A lost exchange invalidates the client; writes are never replayed.
pub struct OwnerLink {
    profile: LocalProfile,
    generation: String,
    client: RwLock<Option<Arc<RootOwnerClient>>>,
}

/// The result of comparing the authenticated owner floor with local history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerBootAuthority {
    Ready,
    MetadataOnly(&'static str),
}

impl OwnerLink {
    pub fn new(profile: LocalProfile) -> Result<Arc<Self>> {
        let facts = super::local_supervisor_facts(&profile)?;
        Ok(Arc::new(Self {
            profile,
            generation: facts.generation().to_string(),
            client: RwLock::new(None),
        }))
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn client(&self) -> Result<Arc<RootOwnerClient>> {
        if let Some(client) = self.client.read().ok().and_then(|guard| guard.as_ref().cloned()) {
            return Ok(client);
        }
        let client = Arc::new(RootOwnerClient::connect(&self.profile, self.generation.clone())?);
        if let Ok(mut guard) = self.client.write() {
            *guard = Some(Arc::clone(&client));
        }
        Ok(client)
    }

    pub fn invalidate(&self) {
        if let Ok(mut guard) = self.client.write() {
            *guard = None;
        }
    }

    /// Idempotent reads reconnect once; mutations must use the client directly.
    fn read<T>(&self, f: impl Fn(&RootOwnerClient) -> Result<T>) -> Result<T> {
        let client = self.client()?;
        match f(&client) {
            Ok(value) => Ok(value),
            Err(first) => {
                self.invalidate();
                let fresh = self.client()?;
                f(&fresh).map_err(|second| {
                    if matches!(second.kind(), "unknown") {
                        Error::unknown(format!("owner recheck failed after reconnect: {} / {}", first.kind(), second.kind()))
                    } else {
                        second
                    }
                })
            }
        }
    }

    pub fn observe(&self, runtime_id: &str) -> Result<RuntimeStatus> {
        self.read(|client| client.observe(runtime_id))
    }

    pub fn recheck(&self, runtime_id: &str, generation: u64) -> Result<QualifiedRuntime> {
        self.read(|client| client.recheck(runtime_id, generation))
    }

    pub fn owner_evidence(&self, runtime_id: &str, generation: u64) -> Result<OwnerEvidence> {
        self.read(|client| client.owner_evidence(runtime_id, generation))
    }

    /// Install the real owner-backed checkpoint writer, including before the owner is online.
    pub fn install_checkpoint(self: &Arc<Self>, store: &Store) -> Result<()> {
        store.install_protected_checkpoint(Arc::new(OwnerCheckpoint { link: Arc::clone(self) }))
    }

    /// Compare an authenticated, profile-bound witness with the actual local ledger.
    pub fn current_authority(&self, store: &Store, require_enabled: bool) -> Result<(PrestartEligibility, OwnerBootAuthority)> {
        let witness = self.client()?.prestart_eligibility()?;
        if witness.workspace_id() != self.profile.workspace_id()
            || witness.database_epoch() != self.profile.database_epoch()
            || witness.restore_state() != "stable"
        {
            return Err(Error::invalid("runtime_unqualified", "the live owner does not authorize this database lineage"));
        }
        let floor = ExternalFloorV1 {
            workspace_id: witness.workspace_id().to_string(),
            min_sequence: witness.min_sequence(),
            min_digest: Digest::try_from(witness.min_digest().to_string())
                .map_err(|_| Error::invalid("runtime_unqualified", "the owner floor is malformed"))?,
        };
        let authority = if require_enabled {
            store.protected_boot_preflight(&floor)?
        } else {
            store.protected_history_preflight(&floor)?
        };
        let authority = match authority {
            BootAuthority::Ready => OwnerBootAuthority::Ready,
            BootAuthority::MetadataOnly(code) => OwnerBootAuthority::MetadataOnly(code),
        };
        Ok((witness, authority))
    }
}

struct OwnerCheckpoint {
    link: Arc<OwnerLink>,
}

impl ProtectedCheckpointWriter for OwnerCheckpoint {
    fn context(&self, workspace_id: &str, database_epoch: Revision) -> Result<ProtectedCheckpointContext> {
        let witness = self.link.read(|client| client.prestart_eligibility())?;
        if witness.workspace_id() != workspace_id
            || witness.database_epoch() != database_epoch.get()
            || witness.restore_state() != "stable"
        {
            return Err(Error::invalid("runtime_unqualified", "the live owner does not authorize this core history"));
        }
        Ok(ProtectedCheckpointContext {
            owner_epoch: Revision::new(witness.owner_epoch()).map_err(|_| Error::invalid("runtime_unqualified", "the owner epoch is malformed"))?,
            minimum_sequence: Counter::new(witness.min_sequence()).map_err(|_| Error::invalid("runtime_unqualified", "the owner minimum is malformed"))?,
            minimum_digest: Digest::try_from(witness.min_digest().to_string()).map_err(|_| Error::invalid("runtime_unqualified", "the owner minimum is malformed"))?,
        })
    }

    fn checkpoint(&self, checkpoint: &CheckpointV1, prefix: &[LedgerRecordV1]) -> Result<CheckpointAckV1> {
        self.link.client()?.checkpoint(checkpoint, prefix, 1)
    }

    fn acknowledged_head(&self, checkpoint: &CheckpointV1) -> Result<CheckpointAckV1> {
        self.link.client()?.checkpoint_receipt(checkpoint)
    }
}
