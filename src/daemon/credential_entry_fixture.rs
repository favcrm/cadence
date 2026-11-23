//! In-process owner for the credential-entry acceptance rig only. It supplies no
//! agent runtime, provider execution, or RPC shortcuts.
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use cadence_core::connection_dispatch::ledger::{
    CheckpointAckV1, CheckpointV1, LedgerRecordV1, LedgerWatermarkV1,
};
use cadence_core::connection_dispatch::primitives::{Digest, Revision};
use serde_json::{json, Value};

use super::protected_connections::{ManagementEvidence, OwnerEligibility, ProtectedOwnerClient};
use super::ServeOptions;
use crate::error::{Error, Result};
use crate::protected_runtime::ops::RuntimeCaller;
use crate::store::connection_dispatch::{
    BootAuthority, ExternalFloorV1, ProtectedCheckpointContext, ProtectedCheckpointWriter,
};
use crate::store::Store;

#[doc(hidden)]
pub struct Fixture {
    pub(super) origin: String,
    owner: OnceLock<Arc<Owner>>,
}

struct Owner {
    evidence: ManagementEvidence,
    watermark: Mutex<LedgerWatermarkV1>,
    receipts: Mutex<HashMap<u64, Digest>>,
}

impl Fixture {
    pub(super) fn install_store(&self, store: &Store) -> Result<()> {
        let authority = store.protected_authority()?;
        let workspace = store.connection_workspace_id()?;
        if workspace != authority.workspace_id {
            return Err(Error::rejected("fixture workspace identity mismatch"));
        }
        let epoch = authority.database_epoch;
        let watermark = LedgerWatermarkV1::new(epoch);
        let floor = ExternalFloorV1 {
            workspace_id: workspace.clone(),
            min_sequence: watermark.minimum_sequence.get(),
            min_digest: watermark.minimum_digest.clone(),
        };
        let owner = Arc::new(Owner {
            evidence: ManagementEvidence {
                workspace_id: workspace,
                custody_domain: "local".into(),
                database_epoch: epoch,
                owner_epoch: Revision::new(1)
                    .map_err(|_| Error::internal("invalid fixture epoch"))?,
                profile_digest: Digest::try_from(format!("sha256:{}", "a".repeat(64)))
                    .map_err(|_| Error::internal("invalid fixture profile digest"))?,
                supervisor_generation: uuid::Uuid::new_v4().simple().to_string(),
                fenced: false,
            },
            watermark: Mutex::new(watermark),
            receipts: Mutex::new(HashMap::new()),
        });
        store.install_protected_checkpoint(owner.clone())?;
        if !matches!(
            store.protected_history_preflight(&floor)?,
            BootAuthority::Ready
        ) {
            return Err(Error::rejected(
                "fixture history does not match owner watermark",
            ));
        }
        self.owner
            .set(owner)
            .map_err(|_| Error::internal("fixture owner already installed"))
    }

    pub(super) fn owner(&self) -> Result<Arc<dyn ProtectedOwnerClient>> {
        Ok(self
            .owner
            .get()
            .ok_or_else(|| Error::internal("fixture owner unavailable"))?
            .clone())
    }

    pub(super) fn evidence(&self) -> Result<ManagementEvidence> {
        self.owner()?.management_evidence()
    }
}

impl ProtectedOwnerClient for Owner {
    fn current_eligibility(&self, _: &RuntimeCaller) -> Result<OwnerEligibility> {
        Err(Error::rejected("fixture has no agent runtime"))
    }
    fn management_evidence(&self) -> Result<ManagementEvidence> {
        Ok(self.evidence.clone())
    }
}

impl Owner {
    fn ack(&self, checkpoint: &CheckpointV1) -> Result<CheckpointAckV1> {
        if checkpoint.workspace_id.as_str() != self.evidence.workspace_id
            || checkpoint.database_epoch != self.evidence.database_epoch
            || checkpoint.owner_epoch != Some(self.evidence.owner_epoch)
        {
            return Err(Error::rejected("fixture checkpoint identity mismatch"));
        }
        Ok(CheckpointAckV1 {
            v: 1,
            workspace_id: checkpoint.workspace_id.clone(),
            custody_domain: checkpoint.custody_domain,
            database_epoch: checkpoint.database_epoch,
            sequence: checkpoint.ledger_head.sequence,
            digest: checkpoint.ledger_head.digest.clone(),
        })
    }
}

impl ProtectedCheckpointWriter for Owner {
    fn context(&self, workspace: &str, epoch: Revision) -> Result<ProtectedCheckpointContext> {
        if workspace != self.evidence.workspace_id || epoch != self.evidence.database_epoch {
            return Err(Error::rejected("fixture checkpoint identity mismatch"));
        }
        let floor = self
            .watermark
            .lock()
            .map_err(|_| Error::internal("fixture watermark poisoned"))?;
        Ok(ProtectedCheckpointContext {
            owner_epoch: self.evidence.owner_epoch,
            minimum_sequence: floor.minimum_sequence,
            minimum_digest: floor.minimum_digest.clone(),
        })
    }
    fn checkpoint(
        &self,
        checkpoint: &CheckpointV1,
        records: &[LedgerRecordV1],
    ) -> Result<CheckpointAckV1> {
        let ack = self.ack(checkpoint)?;
        let mut floor = self
            .watermark
            .lock()
            .map_err(|_| Error::internal("fixture watermark poisoned"))?;
        let mut next = floor.clone();
        next.accept_checkpoint(checkpoint, records)
            .map_err(|_| Error::rejected("fixture checkpoint chain mismatch"))?;
        *floor = next;
        self.receipts
            .lock()
            .map_err(|_| Error::internal("fixture receipts poisoned"))?
            .insert(
                checkpoint.ledger_head.sequence.get(),
                checkpoint.ledger_head.digest.clone(),
            );
        Ok(ack)
    }
    fn acknowledged_head(&self, checkpoint: &CheckpointV1) -> Result<CheckpointAckV1> {
        let ack = self.ack(checkpoint)?;
        if self
            .receipts
            .lock()
            .map_err(|_| Error::internal("fixture receipts poisoned"))?
            .get(&checkpoint.ledger_head.sequence.get())
            != Some(&checkpoint.ledger_head.digest)
        {
            return Err(Error::rejected("fixture has no checkpoint receipt"));
        }
        Ok(ack)
    }
}

// Synthetic metadata only: it exercises existing protected token enrollment;
// it is not an OpenRouter transport and every execution is refused.
struct TokenDescriptor(crate::contract_fixture::ToolTable);
impl crate::platform::PlatformAdapter for TokenDescriptor {
    fn table(&self) -> &crate::contract_fixture::ToolTable {
        &self.0
    }
    fn direct_call(&self, _: &str) -> bool {
        false
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.0.manifest_version.clone()
    }
    fn connection_descriptor(&self) -> Option<crate::platform::connections::ProviderDescriptor> {
        use crate::platform::connections::{
            CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
        };
        Some(ProviderDescriptor {
            schema: 1,
            provider: "openrouter".into(),
            revision: "fixture-token/1".into(),
            enrollment_shapes: vec!["token".into()],
            builtin_accounts: vec![],
            action_mappings: vec![],
            capabilities: vec![CapabilityDescriptor {
                id: "fixture.read".into(),
                version: 1,
                tools: vec!["fixture.read".into()],
                scopes: vec![],
                effect: "read".into(),
                semantics: CapabilitySemantics::MetadataRead,
            }],
        })
    }
    fn preview(&self, _: &str, _: &str, _: &Value) -> String {
        "unavailable".into()
    }
    fn execute(
        &self,
        _: &[u8],
        _: &str,
        _: &Value,
        _: &str,
        _: Option<&str>,
    ) -> std::result::Result<Value, String> {
        Err("fixture has no transport".into())
    }
    fn read_back(&self, _: &str, _: &Value) -> crate::contract_fixture::Verified {
        crate::contract_fixture::Verified::Unknown
    }
    fn source_hash(&self, _: &str, _: &str) -> Option<String> {
        None
    }
}

#[doc(hidden)]
pub fn configure_credential_entry_fixture(
    opts: &mut ServeOptions,
    root: &Path,
    entry_origin: &str,
) -> Result<()> {
    if !crate::board_identity::dedicated_issuer_ok(entry_origin) {
        return Err(Error::rejected("fixture requires a bare HTTPS origin"));
    }
    let state = root.join("state");
    let keys = root.join("keys");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&state)?;
    std::fs::DirBuilder::new().mode(0o700).create(&keys)?;
    let key_path = keys.join("key");
    let mut key = zeroize::Zeroizing::new([0u8; 32]);
    getrandom::fill(key.as_mut())
        .map_err(|_| Error::internal("fixture key entropy unavailable"))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)?;
    file.write_all(key.as_slice())?;
    file.sync_all()?;
    std::fs::File::open(&keys)?.sync_all()?;
    let config = crate::platform::encrypted_sqlite::EncryptedCustodyConfig {
        key_file: key_path,
        vault_dir: state.join("vault"),
    };
    let mut record = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(state.join(crate::platform::encrypted_config::RECORD))?;
    record.write_all(&serde_json::to_vec(
        &json!({"key_file": config.key_file, "vault_dir": config.vault_dir}),
    )?)?;
    record.sync_all()?;
    std::fs::File::open(&state)?.sync_all()?;
    opts.encrypted_custody = Some(config);
    opts.test_seam = true;
    opts.credential_entry_fixture = Some(Arc::new(Fixture {
        origin: entry_origin.into(),
        owner: OnceLock::new(),
    }));
    let table = crate::contract_fixture::ToolTable {
        platform: "openrouter".into(),
        manifest_version: Some("fixture-token/1".into()),
        tools: vec![crate::contract_fixture::ToolDecl {
            tool: "fixture.read".into(),
            effect: Some("read".into()),
            scopes: vec![],
            label: None,
            connected_read: false,
        }],
    };
    opts.platforms
        .insert("openrouter".into(), Arc::new(TokenDescriptor(table)));
    Ok(())
}
