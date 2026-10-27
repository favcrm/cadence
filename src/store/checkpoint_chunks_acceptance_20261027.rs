//! Independent CAD-1085 acceptance: one checkpoint exchange carries one bounded
//! suffix, never the whole retained history.
//!
//! Child module of `store::connection_dispatch`. Builds a real hash-linked
//! ledger chain with the shared constructors and checks the chunk planner's
//! bounds, contiguity and final-head binding. No owner socket is contacted.

use cadence_core::connection_dispatch::ledger::{
    extend_ledger_v1, genesis_ledger_head_v1, CustodyDomainV1, LedgerMutationKindV1,
    LedgerMutationV1,
};
use cadence_core::connection_dispatch::{Digest, Revision};

use super::{checkpoint_chunks, LedgerRecordV1, CHECKPOINT_MAX_RECORDS};

const WORKSPACE: &str = "abababababababababababababababab";
const DATABASE_EPOCH: u64 = 1;

fn digest(byte: char) -> Digest {
    Digest::try_from(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

fn chain(len: usize) -> Vec<LedgerRecordV1> {
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    let mut head = genesis_ledger_head_v1(epoch);
    let mut records = Vec::with_capacity(len);
    for index in 0..len {
        let mutation = LedgerMutationV1 {
            v: 1,
            database_epoch: epoch,
            kind: LedgerMutationKindV1::Claim,
            refs: vec![digest(
                char::from_digit(index as u32 % 16, 16).expect("hex digit"),
            )],
        };
        let next = extend_ledger_v1(&head, &mutation).expect("chain head");
        records.push(LedgerRecordV1 {
            v: 1,
            sequence: next.sequence,
            database_epoch: epoch,
            previous_digest: head.digest.clone(),
            kind: mutation.kind,
            refs: mutation.refs.clone(),
            digest: next.digest.clone(),
        });
        head = next;
    }
    records
}

/// 130 records split into 64/64/2, contiguous, and only the last chunk names
/// the committed head.
#[test]
fn long_history_is_split_into_bounded_contiguous_suffixes() {
    let records = chain(130);
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    let chunks = checkpoint_chunks(&records, WORKSPACE, epoch, &digest('3'))
        .expect("a real chain is chunkable");
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].1.len(), CHECKPOINT_MAX_RECORDS);
    assert_eq!(chunks[1].1.len(), CHECKPOINT_MAX_RECORDS);
    assert_eq!(chunks[2].1.len(), 130 - 2 * CHECKPOINT_MAX_RECORDS);

    for pair in chunks.windows(2) {
        let previous = pair[0].1.last().expect("non-empty chunk");
        let next = pair[1].1.first().expect("non-empty chunk");
        assert_eq!(
            next.previous_digest, previous.digest,
            "the next suffix must continue the same chain"
        );
        assert_eq!(next.sequence.get(), previous.sequence.get() + 1);
    }
    for (checkpoint, chunk) in &chunks {
        let last = chunk.last().expect("non-empty chunk");
        assert_eq!(checkpoint.ledger_head.sequence, last.sequence);
        assert_eq!(checkpoint.ledger_head.digest, last.digest);
        assert_eq!(checkpoint.custody_domain, CustodyDomainV1::Local);
    }
    let committed = records.last().expect("non-empty chain");
    let (final_checkpoint, final_chunk) = chunks.last().expect("final chunk");
    assert_eq!(final_chunk.len(), 2);
    assert_eq!(final_checkpoint.ledger_head.sequence, committed.sequence);
    assert_eq!(final_checkpoint.ledger_head.digest, committed.digest);
}

/// An empty history is not a checkpoint.
#[test]
fn empty_history_is_refused() {
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    checkpoint_chunks(&[], WORKSPACE, epoch, &digest('3'))
        .expect_err("an empty suffix must be refused");
}

/// A single record larger than one bounded frame is refused instead of being
/// truncated or spread across frames.
#[test]
fn oversized_single_record_is_refused() {
    let mut records = chain(1);
    let refs = vec![digest('e'); 400];
    records[0].refs = refs;
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    checkpoint_chunks(&records, WORKSPACE, epoch, &digest('3'))
        .expect_err("a record larger than one frame must be refused");
}
