//! Independent CAD-1085 acceptance: on a protected deployment no side-store
//! authorizing write commits without the authenticated checkpoint lane, and
//! with the lane installed the change is sealed into the ledger before the
//! write reports success.
//!
//! Child module of `store::connection_dispatch`. Real `Store` schema, real
//! migration-installed journal triggers and the production write path; the
//! owner transport is a fake.

use cadence_core::connection_dispatch::ledger::{
    genesis_ledger_head_v1, CheckpointAckV1, CheckpointV1, LedgerRecordV1,
};
use cadence_core::connection_dispatch::{Counter, Revision};

use super::{
    ledger_head_in, ledger_record_in, ProtectedCheckpointContext, ProtectedCheckpointWriter, Store,
};
use crate::error::Error;

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

struct AcceptingWriter {
    context: ProtectedCheckpointContext,
}

impl ProtectedCheckpointWriter for AcceptingWriter {
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
        Ok(ack_for(checkpoint))
    }

    fn acknowledged_head(
        &self,
        _checkpoint: &CheckpointV1,
    ) -> crate::error::Result<CheckpointAckV1> {
        Err(Error::rejected("fixture owner holds no receipt"))
    }
}

fn open_store() -> (Store, tempfile::TempDir) {
    let root = tempfile::tempdir().expect("fixture temp dir");
    let db = root.path().join("side-store-authority.sqlite3");
    let store = Store::open(&db).expect("actual production store schema");
    (store, root)
}

fn require_protection(store: &Store) {
    store
        .write_tx(|tx| {
            tx.execute_raw(
                "UPDATE connection_dispatch_config SET protection_required=1 WHERE singleton=1",
                [],
            )?;
            Ok(())
        })
        .expect("the selection is written before the lane exists");
}

fn insert_default(store: &Store) -> crate::error::Result<()> {
    store.write_tx(|tx| {
        tx.execute_raw(
            "INSERT INTO platform_defaults(project,platform,account,set_at,by) \
             VALUES('fixture-project','devin','fixture-account',1,'acceptance')",
            [],
        )?;
        Ok(())
    })
}

fn ledger_sequence(store: &Store) -> u64 {
    store
        .read_tx(|conn| ledger_head_in(&*conn))
        .expect("local head is readable")
        .sequence
        .get()
}

/// A protected deployment refuses the side-store authorizing write while no
/// authenticated lane is installed.
#[test]
fn protected_side_store_refuses_writes_without_the_lane() {
    let (store, _root) = open_store();
    require_protection(&store);

    let baseline = ledger_sequence(&store);
    let error = insert_default(&store).expect_err("a protected side-store write needs the lane");
    assert!(
        error.to_string().contains("authenticated checkpoint lane"),
        "unexpected refusal: {error}"
    );
    assert_eq!(
        ledger_sequence(&store),
        baseline,
        "nothing was sealed or committed"
    );
    let rows: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM platform_defaults", [], |row| {
            row.get(0)
        })
        .expect("fixture read");
    assert_eq!(rows, 0, "the refused write must not persist");
}

/// With the authenticated lane installed the same write commits, its row is
/// sealed into one ledger record, and the checkpoint is acknowledged before
/// the write returns.
#[test]
fn authenticated_lane_seals_the_side_store_change() {
    let (store, _root) = open_store();
    require_protection(&store);
    let epoch = Revision::new(1).expect("positive owner epoch");
    let genesis = genesis_ledger_head_v1(epoch);
    store
        .install_protected_checkpoint(std::sync::Arc::new(AcceptingWriter {
            context: ProtectedCheckpointContext {
                owner_epoch: epoch,
                minimum_sequence: Counter::zero(),
                minimum_digest: genesis.digest,
            },
        }))
        .expect("fixture transport installs once");

    let baseline = ledger_sequence(&store);
    insert_default(&store).expect("the authenticated lane admits the side-store write");

    assert_eq!(
        ledger_sequence(&store),
        baseline + 1,
        "the change sealed one record"
    );
    let record = store
        .read_tx(|conn| ledger_record_in(&*conn, baseline + 1))
        .expect("ledger read")
        .expect("the sealed record exists");
    assert!(
        !record.refs.is_empty(),
        "the sealed record must reference the changed authority rows"
    );
    let rows: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM platform_defaults", [], |row| {
            row.get(0)
        })
        .expect("fixture read");
    assert_eq!(rows, 1, "the admitted write persisted");
    assert!(!store.admission_fenced());
}
