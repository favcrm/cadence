//! Independent CAD-1085 acceptance: a protected writer reports success only
//! after the owner acknowledged its exact checkpoint; an unacknowledged or
//! mismatched exchange is `OutcomeUnknown` and fences admission.
//!
//! Child module of `store::connection_dispatch`. Uses a real `Store` schema and
//! a fake checkpoint transport; no owner socket is contacted.

use cadence_core::connection_dispatch::ledger::{
    genesis_ledger_head_v1, CheckpointAckV1, CheckpointV1, LedgerHeadV1, LedgerMutationKindV1,
    LedgerRecordV1,
};
use cadence_core::connection_dispatch::Counter;
use cadence_core::connection_dispatch::{Digest, Revision};

use super::{
    append_record_in, ledger_head_in, ProtectedCheckpointContext, ProtectedCheckpointWriter, Store,
};
use crate::error::Error;

fn digest(byte: char) -> Digest {
    Digest::try_from(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

fn ack_for(checkpoint: &CheckpointV1) -> CheckpointAckV1 {
    CheckpointAckV1 {
        v: 1,
        workspace_id: checkpoint.workspace_id.clone(),
        custody_domain: checkpoint.custody_domain,
        database_epoch: checkpoint.database_epoch,
        sequence: checkpoint.ledger_head.sequence,
        digest: checkpoint.ledger_head.digest.clone(),
    }
}

#[derive(Clone, Copy)]
enum Behaviour {
    Accept,
    TransportFailure,
    WrongDigest,
}

struct FakeWriter {
    context: ProtectedCheckpointContext,
    behaviour: Behaviour,
}

impl ProtectedCheckpointWriter for FakeWriter {
    fn context(
        &self,
        _workspace_id: &str,
        _database_epoch: Revision,
    ) -> crate::error::Result<ProtectedCheckpointContext> {
        Ok(self.context.clone())
    }

    fn checkpoint(
        &self,
        checkpoint: &CheckpointV1,
        _records: &[LedgerRecordV1],
    ) -> crate::error::Result<CheckpointAckV1> {
        match self.behaviour {
            Behaviour::Accept => Ok(ack_for(checkpoint)),
            Behaviour::TransportFailure => {
                Err(Error::internal("fixture owner transport is unavailable"))
            }
            Behaviour::WrongDigest => {
                let mut ack = ack_for(checkpoint);
                ack.digest = digest('f');
                Ok(ack)
            }
        }
    }

    fn acknowledged_head(
        &self,
        _checkpoint: &CheckpointV1,
    ) -> crate::error::Result<CheckpointAckV1> {
        Err(Error::rejected("fixture owner holds no receipt"))
    }
}

/// Open a store and append one real ledger record through the production
/// write path, then return the committed head and a genesis watermark context.
fn seeded_store() -> (Store, String, LedgerHeadV1, ProtectedCheckpointContext) {
    let root = tempfile::tempdir().expect("fixture temp dir");
    let db = root.path().join("checkpoint-ordering.sqlite3");
    let store = Store::open(&db).expect("actual production store schema");
    let workspace = store.connection_workspace_id().expect("workspace id");
    let head = store
        .write_tx(|tx| {
            let head = ledger_head_in(tx)?;
            append_record_in(tx, &head, LedgerMutationKindV1::Claim, vec![digest('2')])
        })
        .expect("the production write path appends one ledger record");
    let genesis = genesis_ledger_head_v1(head.database_epoch);
    let context = ProtectedCheckpointContext {
        owner_epoch: Revision::new(1).expect("positive owner epoch"),
        minimum_sequence: Counter::zero(),
        minimum_digest: genesis.digest,
    };
    (store, workspace, head, context)
}

fn fence_row(store: &Store) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT fenced FROM connection_dispatch_config WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .expect("fixture fence row")
}

/// The exact acknowledgement is what turns the exchange into success.
#[test]
fn committed_head_is_acknowledged_before_success() {
    let (store, workspace, _head, context) = seeded_store();
    store
        .install_protected_checkpoint(std::sync::Arc::new(FakeWriter {
            context,
            behaviour: Behaviour::Accept,
        }))
        .expect("fixture transport installs once");
    store
        .protected_checkpoint_head(
            &workspace,
            Revision::new(1).expect("owner epoch"),
            digest('3'),
        )
        .expect("an exact acknowledgement reports success");
    assert!(!store.admission_fenced());
    assert_eq!(fence_row(&store), 0);
}

/// A transport failure after the writer committed is Unknown and fences.
#[test]
fn transport_failure_is_unknown_and_fences() {
    let (store, workspace, _head, context) = seeded_store();
    store
        .install_protected_checkpoint(std::sync::Arc::new(FakeWriter {
            context,
            behaviour: Behaviour::TransportFailure,
        }))
        .expect("fixture transport installs once");
    let error = store
        .protected_checkpoint_head(
            &workspace,
            Revision::new(1).expect("owner epoch"),
            digest('3'),
        )
        .expect_err("a failed checkpoint cannot report success");
    assert!(
        matches!(error, Error::OutcomeUnknown(_)),
        "unexpected error: {error:?}"
    );
    assert!(store.admission_fenced());
    assert_eq!(fence_row(&store), 1);
}

/// A reply that acknowledges a different head is not success either.
#[test]
fn mismatched_acknowledgement_is_unknown_and_fences() {
    let (store, workspace, _head, context) = seeded_store();
    store
        .install_protected_checkpoint(std::sync::Arc::new(FakeWriter {
            context,
            behaviour: Behaviour::WrongDigest,
        }))
        .expect("fixture transport installs once");
    let error = store
        .protected_checkpoint_head(
            &workspace,
            Revision::new(1).expect("owner epoch"),
            digest('3'),
        )
        .expect_err("a mismatched acknowledgement cannot report success");
    assert!(
        matches!(error, Error::OutcomeUnknown(_)),
        "unexpected error: {error:?}"
    );
    assert!(store.admission_fenced());
    assert_eq!(fence_row(&store), 1);
}

/// The equal-head path also requires the owner's real acknowledgement.
#[test]
fn equal_head_requires_the_owner_acknowledgement() {
    let (store, workspace, head, context) = seeded_store();
    let equal_context = ProtectedCheckpointContext {
        owner_epoch: context.owner_epoch,
        minimum_sequence: head.sequence,
        minimum_digest: head.digest.clone(),
    };
    store
        .install_protected_checkpoint(std::sync::Arc::new(FakeWriter {
            context: equal_context,
            behaviour: Behaviour::TransportFailure,
        }))
        .expect("fixture transport installs once");
    let error = store
        .protected_checkpoint_head(
            &workspace,
            Revision::new(1).expect("owner epoch"),
            digest('3'),
        )
        .expect_err("an unacknowledged equal head cannot report success");
    assert!(matches!(error, Error::OutcomeUnknown(_)));
    assert!(store.admission_fenced());
    assert_eq!(fence_row(&store), 1);
}
