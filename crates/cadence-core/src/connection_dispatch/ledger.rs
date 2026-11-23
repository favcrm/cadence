//! The protected ledger: one canonical hash chain over metadata-only
//! authorization, claim, reservation, settlement and fence changes, plus the
//! monotonic external watermark that fences authentic old snapshots.
//!
//! The chain binds the database epoch, a checked sequence and the previous
//! digest. It never carries plaintext credentials or an unkeyed credential
//! digest.
use crate::connection_dispatch::canonical::{material_digest_value, PROTECTED_LEDGER_DOMAIN};
use crate::connection_dispatch::dto::{DispatchError, ErrorCodeV1};
use crate::connection_dispatch::primitives::{Counter, Digest, Ident, Revision};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const LEDGER_CONTRACT_VERSION: u32 = 1;
/// The native/local protected custody domain literal (r3 wire value).
pub const CUSTODY_DOMAIN_LOCAL: &str = "local";
/// The hosted protected custody domain literal. A hosted checkpoint binds the
/// custody epoch and carries no owner epoch.
pub const CUSTODY_DOMAIN_HOSTED: &str = "hosted";

/// The checkpoint's custody-domain selector: exactly `local` (native owner)
/// or `hosted` (private custody deployment). It is NOT the Connection's data
/// custody domain, so the two must never be compared as strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CustodyDomainV1 {
    Local,
    Hosted,
}

impl CustodyDomainV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            CustodyDomainV1::Local => CUSTODY_DOMAIN_LOCAL,
            CustodyDomainV1::Hosted => CUSTODY_DOMAIN_HOSTED,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LedgerMutationKindV1 {
    Credential,
    Grant,
    Authorization,
    Policy,
    Claim,
    Reservation,
    Settlement,
    Fence,
    Restore,
}

impl LedgerMutationKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            LedgerMutationKindV1::Credential => "credential",
            LedgerMutationKindV1::Grant => "grant",
            LedgerMutationKindV1::Authorization => "authorization",
            LedgerMutationKindV1::Policy => "policy",
            LedgerMutationKindV1::Claim => "claim",
            LedgerMutationKindV1::Reservation => "reservation",
            LedgerMutationKindV1::Settlement => "settlement",
            LedgerMutationKindV1::Fence => "fence",
            LedgerMutationKindV1::Restore => "restore",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LedgerHeadV1 {
    pub sequence: Counter,
    pub database_epoch: Revision,
    pub digest: Digest,
}

impl LedgerHeadV1 {
    /// A head may be the genesis head (sequence 0, epoch-bound canonical
    /// digest); record sequences themselves are always positive.
    pub fn validate(&self) -> Result<(), LedgerError> {
        Ok(())
    }
}

/// The canonical epoch-bound genesis head. A protected database starts here
/// (sequence 0), and the first record extends it to sequence 1.
pub fn genesis_ledger_head_v1(database_epoch: Revision) -> LedgerHeadV1 {
    let material = serde_json::json!({
        "v": LEDGER_CONTRACT_VERSION,
        "kind": "genesis",
        "sequence": 0,
        "database_epoch": database_epoch,
    });
    LedgerHeadV1 {
        sequence: Counter::zero(),
        database_epoch,
        digest: material_digest_value(PROTECTED_LEDGER_DOMAIN, &material),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LedgerMutationV1 {
    pub v: u32,
    pub database_epoch: Revision,
    pub kind: LedgerMutationKindV1,
    pub refs: Vec<Digest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LedgerRecordV1 {
    pub v: u32,
    pub sequence: Counter,
    pub database_epoch: Revision,
    pub previous_digest: Digest,
    pub kind: LedgerMutationKindV1,
    pub refs: Vec<Digest>,
    pub digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerError {
    Malformed,
    SequenceExhausted,
    EpochRegression,
    Stale,
    Mismatch,
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LedgerError::Malformed => "ledger record is malformed",
            LedgerError::SequenceExhausted => "ledger sequence is exhausted",
            LedgerError::EpochRegression => "ledger epoch cannot move backwards",
            LedgerError::Stale => "ledger checkpoint is older than the watermark",
            LedgerError::Mismatch => "ledger prefix does not match",
        })
    }
}
impl std::error::Error for LedgerError {}

impl From<LedgerError> for DispatchError {
    fn from(_: LedgerError) -> Self {
        DispatchError::new(ErrorCodeV1::LedgerMismatch)
    }
}

fn record_digest(
    sequence: Counter,
    database_epoch: Revision,
    previous_digest: &Digest,
    kind: LedgerMutationKindV1,
    refs: &[Digest],
) -> Digest {
    let material = serde_json::json!({
        "v": LEDGER_CONTRACT_VERSION,
        "sequence": sequence,
        "database_epoch": database_epoch,
        "previous_digest": previous_digest,
        "kind": kind,
        "refs": refs,
    });
    material_digest_value(PROTECTED_LEDGER_DOMAIN, &material)
}

/// Append one record to the chain. The database epoch may advance but never
/// move backwards; the sequence increments with checked arithmetic.
pub fn extend_ledger_v1(
    head: &LedgerHeadV1,
    mutation: &LedgerMutationV1,
) -> Result<LedgerHeadV1, LedgerError> {
    head.validate()?;
    if mutation.v != LEDGER_CONTRACT_VERSION {
        return Err(LedgerError::Malformed);
    }
    if mutation.database_epoch.get() < head.database_epoch.get() {
        return Err(LedgerError::EpochRegression);
    }
    let sequence = head
        .sequence
        .checked_next()
        .map_err(|_| LedgerError::SequenceExhausted)?;
    let digest = record_digest(
        sequence,
        mutation.database_epoch,
        &head.digest,
        mutation.kind,
        &mutation.refs,
    );
    Ok(LedgerHeadV1 {
        sequence,
        database_epoch: mutation.database_epoch,
        digest,
    })
}

/// Verify a contiguous chain segment and return its head. The first record's
/// `previous_digest` is the caller's authoritative anchor.
pub fn verify_ledger_chain_v1(records: &[LedgerRecordV1]) -> Result<LedgerHeadV1, LedgerError> {
    let first = records.first().ok_or(LedgerError::Malformed)?;
    if first.v != LEDGER_CONTRACT_VERSION {
        return Err(LedgerError::Malformed);
    }
    // The segment is anchored by its first record's `previous_digest`; the
    // caller compares that anchor to its own authoritative head.
    let mut previous_sequence: Option<Counter> = None;
    let mut previous_digest = first.previous_digest.clone();
    let mut epoch = first.database_epoch;
    let mut head = LedgerHeadV1 {
        sequence: first.sequence,
        database_epoch: first.database_epoch,
        digest: first.digest.clone(),
    };
    for record in records {
        if record.v != LEDGER_CONTRACT_VERSION || record.sequence.get() == 0 {
            return Err(LedgerError::Malformed);
        }
        if let Some(previous) = previous_sequence {
            if record.sequence
                != previous
                    .checked_next()
                    .map_err(|_| LedgerError::SequenceExhausted)?
            {
                return Err(LedgerError::Mismatch);
            }
        }
        if record.previous_digest != previous_digest {
            return Err(LedgerError::Mismatch);
        }
        if record.database_epoch.get() < epoch.get() {
            return Err(LedgerError::EpochRegression);
        }
        let digest = record_digest(
            record.sequence,
            record.database_epoch,
            &record.previous_digest,
            record.kind,
            &record.refs,
        );
        if digest != record.digest {
            return Err(LedgerError::Mismatch);
        }
        previous_sequence = Some(record.sequence);
        previous_digest = record.digest.clone();
        epoch = record.database_epoch;
        head = LedgerHeadV1 {
            sequence: record.sequence,
            database_epoch: record.database_epoch,
            digest,
        };
    }
    Ok(head)
}

/// The external restore watermark: monotonic and never lowered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LedgerWatermarkV1 {
    pub minimum_sequence: Counter,
    pub minimum_digest: Digest,
    pub database_epoch: Revision,
}

impl LedgerWatermarkV1 {
    pub fn new(database_epoch: Revision) -> Self {
        let genesis = genesis_ledger_head_v1(database_epoch);
        Self {
            minimum_sequence: genesis.sequence,
            minimum_digest: genesis.digest,
            database_epoch,
        }
    }

    /// Advance only with a chain segment that starts exactly at the current
    /// watermark. A current higher sequence without its immutable prefix
    /// refuses; so does an equal sequence with a different digest.
    pub fn advance_from_chain(
        &mut self,
        records: &[LedgerRecordV1],
    ) -> Result<LedgerHeadV1, LedgerError> {
        let head = verify_ledger_chain_v1(records)?;
        let first = records.first().ok_or(LedgerError::Malformed)?;
        if first.sequence
            != self
                .minimum_sequence
                .checked_next()
                .map_err(|_| LedgerError::SequenceExhausted)?
        {
            return Err(LedgerError::Stale);
        }
        if first.previous_digest != self.minimum_digest {
            return Err(LedgerError::Mismatch);
        }
        if head.database_epoch.get() < self.database_epoch.get() {
            return Err(LedgerError::EpochRegression);
        }
        self.minimum_sequence = head.sequence;
        self.minimum_digest = head.digest.clone();
        self.database_epoch = head.database_epoch;
        Ok(head)
    }

    /// Accept a checkpoint only when it equals or extends the watermark with
    /// proven inclusion; equal sequence/different digest and older sequences
    /// refuse.
    pub fn accept_checkpoint(
        &mut self,
        checkpoint: &CheckpointV1,
        prefix: &[LedgerRecordV1],
    ) -> Result<(), LedgerError> {
        checkpoint.validate()?;
        if checkpoint.ledger_head.sequence < self.minimum_sequence {
            return Err(LedgerError::Stale);
        }
        if checkpoint.ledger_head.sequence == self.minimum_sequence {
            if checkpoint.ledger_head.digest != self.minimum_digest {
                return Err(LedgerError::Mismatch);
            }
            if checkpoint.ledger_head.database_epoch.get() != self.database_epoch.get() {
                return Err(LedgerError::EpochRegression);
            }
            return Ok(());
        }
        let head = self.advance_from_chain(prefix)?;
        if head != checkpoint.ledger_head {
            return Err(LedgerError::Mismatch);
        }
        Ok(())
    }
}

/// A private owner/custody-control checkpoint. Not a runtime permit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckpointV1 {
    pub v: u32,
    pub workspace_id: Ident,
    pub custody_domain: CustodyDomainV1,
    pub database_epoch: Revision,
    pub owner_epoch: Option<Revision>,
    pub custody_epoch: Option<Revision>,
    pub ledger_head: LedgerHeadV1,
    pub runtime_binding_digest: Digest,
}

impl CheckpointV1 {
    pub fn validate(&self) -> Result<(), LedgerError> {
        if self.v != LEDGER_CONTRACT_VERSION {
            return Err(LedgerError::Malformed);
        }
        self.ledger_head.validate()?;
        if self.ledger_head.database_epoch != self.database_epoch {
            return Err(LedgerError::Mismatch);
        }
        match self.custody_domain {
            // Native/local custody binds the owner epoch and only that.
            CustodyDomainV1::Local => {
                if self.owner_epoch.is_none() || self.custody_epoch.is_some() {
                    return Err(LedgerError::Malformed);
                }
            }
            // Hosted custody binds the custody epoch and only that.
            CustodyDomainV1::Hosted => {
                if self.custody_epoch.is_none() || self.owner_epoch.is_some() {
                    return Err(LedgerError::Malformed);
                }
            }
        }
        Ok(())
    }
}

/// The exact reply the external watermark returns for a checkpoint. Only a
/// matching acknowledgement promotes the prepared buffers into execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckpointAckV1 {
    pub v: u32,
    pub workspace_id: Ident,
    pub custody_domain: CustodyDomainV1,
    pub database_epoch: Revision,
    pub sequence: Counter,
    pub digest: Digest,
}

impl CheckpointAckV1 {
    pub fn validate(&self) -> Result<(), LedgerError> {
        if self.v != LEDGER_CONTRACT_VERSION || self.sequence.get() == 0 {
            return Err(LedgerError::Malformed);
        }
        Ok(())
    }
}
