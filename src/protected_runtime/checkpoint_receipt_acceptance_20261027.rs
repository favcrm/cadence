//! Independent CAD-1085 acceptance: bounded checkpoint suffixes and read-only
//! historical owner receipts.
//!
//! Child module of `protected_runtime::store`. The production schema and the
//! real watermark/checkpoint transitions run against a real in-memory SQLite
//! connection. No supervisor socket, Docker retirement or broker ACK is
//! supplied: the store contract alone is exercised.

use std::path::PathBuf;
use std::sync::Mutex;

use cadence_core::connection_dispatch::ledger::{
    extend_ledger_v1, genesis_ledger_head_v1, CustodyDomainV1, LedgerHeadV1, LedgerMutationKindV1,
    LedgerMutationV1,
};
use cadence_core::connection_dispatch::{Digest, Ident, Revision};
use rusqlite::Connection;

use super::{CheckpointV1, LedgerRecordV1, OwnerStore, SCHEMA_V1};

const WORKSPACE: &str = "abababababababababababababababab";
const DATABASE_EPOCH: u64 = 1;

fn memory_owner() -> OwnerStore {
    let conn = Connection::open_in_memory().expect("actual isolated SQLite connection");
    conn.execute_batch(SCHEMA_V1)
        .expect("install the actual production owner schema");
    let store = OwnerStore {
        conn: Mutex::new(conn),
        path: PathBuf::from(":memory:"),
        workspace_id: WORKSPACE.to_string(),
        boot_id: "checkpoint-receipt-first-boot".to_string(),
    };
    store
        .begin_boot(DATABASE_EPOCH, 1)
        .expect("actual initial production boot transition");
    store
}

fn digest(byte: char) -> Digest {
    Digest::try_from(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

fn mutation(kind: LedgerMutationKindV1, ref_byte: char) -> LedgerMutationV1 {
    LedgerMutationV1 {
        v: 1,
        database_epoch: Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch"),
        kind,
        refs: vec![digest(ref_byte)],
    }
}

fn checkpoint_for(store: &OwnerStore, head: &LedgerHeadV1) -> CheckpointV1 {
    CheckpointV1 {
        v: 1,
        workspace_id: Ident::try_from(WORKSPACE.to_string()).expect("workspace identifier"),
        custody_domain: CustodyDomainV1::Local,
        database_epoch: Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch"),
        owner_epoch: Some(
            Revision::new(store.owner_epoch().expect("current owner epoch"))
                .expect("positive owner epoch"),
        ),
        custody_epoch: None,
        ledger_head: head.clone(),
        runtime_binding_digest: digest('3'),
    }
}

fn record_for(
    previous: &LedgerHeadV1,
    head: &LedgerHeadV1,
    mutation: &LedgerMutationV1,
) -> LedgerRecordV1 {
    LedgerRecordV1 {
        v: 1,
        sequence: head.sequence,
        database_epoch: Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch"),
        previous_digest: previous.digest.clone(),
        kind: mutation.kind,
        refs: mutation.refs.clone(),
        digest: head.digest.clone(),
    }
}

fn watermark_minimum(store: &OwnerStore) -> u64 {
    let conn = store.conn.lock().expect("fixture owner connection");
    let raw: String = conn
        .query_row(
            "SELECT minimum_sequence FROM owner_watermark WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("the actual watermark row is readable");
    raw.parse().expect("the watermark sequence is a number")
}

/// Commit two real chain heads, then read the older one back as a receipt.
#[test]
fn historical_receipt_is_exact_and_never_advances_the_watermark() {
    let store = memory_owner();
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    let genesis = genesis_ledger_head_v1(epoch);

    let first_mutation = mutation(LedgerMutationKindV1::Claim, '2');
    let first_head = extend_ledger_v1(&genesis, &first_mutation).expect("first chain head");
    let first_record = record_for(&genesis, &first_head, &first_mutation);
    let first_checkpoint = checkpoint_for(&store, &first_head);
    let first_ack = store
        .checkpoint(&first_checkpoint, std::slice::from_ref(&first_record))
        .expect("first checkpoint commits");

    let second_mutation = mutation(LedgerMutationKindV1::Settlement, '4');
    let second_head = extend_ledger_v1(&first_head, &second_mutation).expect("second chain head");
    let second_record = record_for(&first_head, &second_head, &second_mutation);
    let second_checkpoint = checkpoint_for(&store, &second_head);
    store
        .checkpoint(&second_checkpoint, std::slice::from_ref(&second_record))
        .expect("second checkpoint commits");
    let advanced = watermark_minimum(&store);
    assert_eq!(advanced, second_head.sequence.get());

    let receipt = store
        .checkpoint_receipt(&first_checkpoint)
        .expect("a committed historical head yields a receipt");
    assert_eq!(receipt.sequence, first_ack.sequence);
    assert_eq!(receipt.digest, first_ack.digest);
    assert_eq!(
        watermark_minimum(&store),
        advanced,
        "a receipt must not move the watermark"
    );
}

/// A receipt naming an epoch other than the live owner epoch is refused: a
/// restarted owner cannot resurrect a pending operation from a surviving
/// historical row.
#[test]
fn receipt_refuses_a_mismatched_owner_epoch() {
    let store = memory_owner();
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    let genesis = genesis_ledger_head_v1(epoch);
    let mutation = mutation(LedgerMutationKindV1::Claim, '5');
    let head = extend_ledger_v1(&genesis, &mutation).expect("chain head");
    let record = record_for(&genesis, &head, &mutation);
    let checkpoint = checkpoint_for(&store, &head);
    store
        .checkpoint(&checkpoint, std::slice::from_ref(&record))
        .expect("checkpoint commits");

    let mut mismatched = checkpoint.clone();
    mismatched.owner_epoch = Some(
        Revision::new(store.owner_epoch().expect("owner epoch") + 1).expect("positive owner epoch"),
    );
    let error = store
        .checkpoint_receipt(&mismatched)
        .expect_err("a mismatched owner epoch must refuse the receipt");
    assert!(
        error.to_string().contains("epoch does not match"),
        "unexpected refusal: {error}"
    );
}

/// A receipt for a head the owner never committed is refused, and the refused
/// read leaves the watermark untouched.
#[test]
fn receipt_refuses_an_uncommitted_head() {
    let store = memory_owner();
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    let genesis = genesis_ledger_head_v1(epoch);
    let first = mutation(LedgerMutationKindV1::Claim, '6');
    let head = extend_ledger_v1(&genesis, &first).expect("committed chain head");
    let record = record_for(&genesis, &head, &first);
    let checkpoint = checkpoint_for(&store, &head);
    store
        .checkpoint(&checkpoint, std::slice::from_ref(&record))
        .expect("checkpoint commits");
    let advanced = watermark_minimum(&store);

    let uncommitted = mutation(LedgerMutationKindV1::Policy, '7');
    let future_head = extend_ledger_v1(&head, &uncommitted).expect("uncommitted chain head");
    let future_checkpoint = checkpoint_for(&store, &future_head);
    let error = store
        .checkpoint_receipt(&future_checkpoint)
        .expect_err("an uncommitted head must refuse the receipt");
    assert!(
        error.to_string().contains("newer than the owner watermark"),
        "unexpected refusal: {error}"
    );
    assert_eq!(watermark_minimum(&store), advanced);
}

/// The shared chain's equal-head path does not examine prefix records, so the
/// store refuses a non-empty suffix rather than persisting unexamined rows.
#[test]
fn equal_head_with_records_is_refused() {
    let store = memory_owner();
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    let genesis = genesis_ledger_head_v1(epoch);
    let mutation = mutation(LedgerMutationKindV1::Grant, '8');
    let head = extend_ledger_v1(&genesis, &mutation).expect("chain head");
    let record = record_for(&genesis, &head, &mutation);
    let checkpoint = checkpoint_for(&store, &head);
    store
        .checkpoint(&checkpoint, std::slice::from_ref(&record))
        .expect("checkpoint commits");

    let error = store
        .checkpoint(&checkpoint, std::slice::from_ref(&record))
        .expect_err("an equal head with records must be refused");
    assert!(
        error
            .to_string()
            .contains("equal-head checkpoint must not carry ledger records"),
        "unexpected refusal: {error}"
    );

    store
        .checkpoint(&checkpoint, &[])
        .expect("an equal head with an empty suffix is idempotent");
}

/// A head whose sequence matches but whose digest differs is a conflict, not a
/// silent idempotent success.
#[test]
fn conflicting_head_is_refused() {
    let store = memory_owner();
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    let genesis = genesis_ledger_head_v1(epoch);
    let mutation = mutation(LedgerMutationKindV1::Authorization, '9');
    let head = extend_ledger_v1(&genesis, &mutation).expect("chain head");
    let record = record_for(&genesis, &head, &mutation);
    let checkpoint = checkpoint_for(&store, &head);
    store
        .checkpoint(&checkpoint, std::slice::from_ref(&record))
        .expect("checkpoint commits");

    let mut conflicting = checkpoint.clone();
    conflicting.ledger_head.digest = digest('a');
    let error = store
        .checkpoint(&conflicting, &[])
        .expect_err("a conflicting digest must be refused");
    assert!(
        error.to_string().to_lowercase().contains("conflict")
            || error.to_string().contains("does not match"),
        "unexpected refusal: {error}"
    );
}
