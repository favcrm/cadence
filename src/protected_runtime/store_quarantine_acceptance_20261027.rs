//! Independent CAD-1085 acceptance: epoch changes must not manufacture retirement.
//!
//! Child module of protected_runtime::store. The production schema and real
//! production transitions run against a real in-memory SQLite connection.
//! No broker ACK, Docker retirement, owner capability, or mark_revoked call is
//! supplied. The absence of those confirmations must retain quarantine.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cadence_core::connection_dispatch::ledger::{
    extend_ledger_v1, genesis_ledger_head_v1, CustodyDomainV1, LedgerMutationKindV1,
    LedgerMutationV1,
};
use cadence_core::connection_dispatch::{Digest, Ident, Revision};
use rusqlite::{params, Connection};

use super::{AllocParams, Allocation, CheckpointV1, LedgerRecordV1, OwnerStore, SCHEMA_V1};

const WORKSPACE: &str = "abababababababababababababababab";
const DATABASE_EPOCH: u64 = 1;
const AGENT: &str = "agt-11111111111111111111111111111111";
const PROFILE: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn memory_owner() -> OwnerStore {
    let conn = Connection::open_in_memory().expect("actual isolated SQLite connection");
    conn.execute_batch(SCHEMA_V1)
        .expect("install the actual production owner schema");
    let store = OwnerStore {
        conn: Mutex::new(conn),
        path: PathBuf::from(":memory:"),
        workspace_id: WORKSPACE.to_string(),
        boot_id: "independent-first-boot".to_string(),
    };
    store
        .begin_boot(DATABASE_EPOCH, 1)
        .expect("actual initial production boot transition");
    store
}

fn allocate_one(store: &OwnerStore, session: &str) -> crate::error::Result<Allocation> {
    store.allocate(AllocParams {
        workspace_id: WORKSPACE,
        agent_uid: AGENT,
        session_binding: session,
        server_generation: 1,
        expected_owner_epoch: store.owner_epoch()?,
        database_epoch: DATABASE_EPOCH,
        profile_digest: PROFILE,
        max_runtimes: 1,
        uid_start: 21001,
        uid_end: 21999,
        // allocate records paths but performs no filesystem or Docker action.
        run_dir: Path::new("/unused-cad1085-in-memory-fixture/run"),
    })
}

fn restore_checkpoint(store: &OwnerStore) -> (CheckpointV1, Vec<LedgerRecordV1>) {
    let epoch = Revision::new(DATABASE_EPOCH).expect("valid fixture database epoch");
    let genesis = genesis_ledger_head_v1(epoch);
    let mutation = LedgerMutationV1 {
        v: 1,
        database_epoch: epoch,
        kind: LedgerMutationKindV1::Restore,
        refs: vec![Digest::try_from(format!("sha256:{}", "2".repeat(64)))
            .expect("fixture metadata-only reference")],
    };
    let head = extend_ledger_v1(&genesis, &mutation)
        .expect("the actual shared ledger constructs a valid non-genesis checkpoint");
    let record = LedgerRecordV1 {
        v: 1,
        sequence: head.sequence,
        database_epoch: epoch,
        previous_digest: genesis.digest,
        kind: mutation.kind,
        refs: mutation.refs,
        digest: head.digest.clone(),
    };
    let checkpoint = CheckpointV1 {
        v: 1,
        workspace_id: Ident::try_from(WORKSPACE.to_string()).expect("workspace identifier"),
        custody_domain: CustodyDomainV1::Local,
        database_epoch: epoch,
        owner_epoch: Some(
            Revision::new(store.owner_epoch().expect("current owner epoch"))
                .expect("positive owner epoch"),
        ),
        custody_epoch: None,
        ledger_head: head,
        runtime_binding_digest: Digest::try_from(format!("sha256:{}", "3".repeat(64)))
            .expect("fixture runtime binding digest"),
    };
    (checkpoint, vec![record])
}

type IdentityRows = (Vec<(String, i64, String, i64, String)>, Vec<(i64, String)>);

fn identity_rows(store: &OwnerStore) -> IdentityRows {
    let conn = store.conn.lock().expect("fixture owner connection");
    let mut obligations = conn
        .prepare(
            "SELECT runtime_id,os_uid,container_name,generation,phase \
         FROM runtime_obligations ORDER BY runtime_id",
        )
        .expect("read actual immutable obligation identities");
    let runtime_rows = obligations
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query actual obligation identities")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect obligation identities");
    let mut allocations = conn
        .prepare("SELECT os_uid,runtime_id FROM uid_allocations ORDER BY os_uid")
        .expect("read actual retained UID allocations");
    let uid_rows = allocations
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query actual UID allocations")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect UID allocations");
    (runtime_rows, uid_rows)
}

fn assert_quarantined_and_capacity_held(store: &OwnerStore, first: &Allocation) {
    let row = store
        .get(&first.runtime_id)
        .expect("original obligation remains present");
    assert_eq!(
        row.phase, "revoking",
        "epoch advancement is not confirmed retirement"
    );
    assert_eq!(
        row.owner_epoch, first.owner_epoch,
        "do not adopt/rewrite the old obligation epoch"
    );
    assert_eq!(row.os_uid, first.os_uid);
    assert_eq!(row.container_name, first.container_name);
    assert_eq!(row.generation, first.generation);
    assert!(
        row.container_id.is_none(),
        "fixture reproduces an unconfirmed pre-create obligation"
    );

    let transitions: Vec<(Option<String>, String)> = {
        let conn = store.conn.lock().expect("fixture owner connection");
        let mut statement = conn
            .prepare(
                "SELECT from_phase,to_phase FROM obligation_transitions \
             WHERE runtime_id=?1 ORDER BY id",
            )
            .expect("read append-only production transitions");
        let rows = statement
            .query_map(params![first.runtime_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("query actual transitions")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect actual transitions");
        rows
    };
    assert_eq!(
        transitions,
        vec![
            (None, "allocating".to_string()),
            (Some("allocating".to_string()), "revoking".to_string()),
        ],
        "there must be no synthetic revoked transition without ACK/retirement"
    );

    let witness = store
        .prestart_eligibility()
        .expect("read actual restore state");
    assert_eq!(
        witness.restore_state, "stable",
        "capacity refusal must not be the restoring gate"
    );
    let before = identity_rows(store);
    assert_eq!(before.0.len(), 1);
    assert_eq!(
        before.1,
        vec![(i64::from(first.os_uid), first.runtime_id.clone())]
    );
    let error = allocate_one(store, "second-session")
        .err()
        .expect("unconfirmed quarantine must still consume the only runtime slot");
    assert_eq!(error.kind(), "rejected");
    assert_eq!(
        error.to_string(),
        "runtime limit reached",
        "not an epoch or UID-exhaustion refusal"
    );
    assert_eq!(
        identity_rows(store),
        before,
        "refused allocation must not reuse/release UID or name"
    );
}

#[test]
fn cad1085_owner_restart_and_restore_preserve_unconfirmed_quarantine() {
    // Both independent legs start from an actual non-revoked allocation.
    for restore in [false, true] {
        let mut store = memory_owner();
        let first = allocate_one(&store, "first-session").expect("positive first allocation");
        assert_eq!(first.os_uid, 21001);
        assert_eq!(first.generation, 1);
        assert_eq!(
            store.get(&first.runtime_id).expect("new allocation").phase,
            "allocating"
        );
        let old_epoch = store.owner_epoch().expect("initial owner epoch");

        if restore {
            let (checkpoint, prefix) = restore_checkpoint(&store);
            let new_epoch = store
                .restore_begin(&checkpoint, &prefix, "independent quarantine restore")
                .expect("actual restore_begin validates/checkpoints and advances the owner epoch");
            assert!(new_epoch > old_epoch);
            // Complete only the actual metadata restore. This is not an ACK
            // or a container retirement and must not free the runtime slot.
            store
                .restore_complete(new_epoch)
                .expect("real restore completion with retained ledger proof");
        } else {
            store.boot_id = "independent-second-boot".to_string();
            store
                .begin_boot(DATABASE_EPOCH, 2)
                .expect("actual restart boot transition");
            assert!(store.owner_epoch().expect("restarted owner epoch") > old_epoch);
        }
        assert_quarantined_and_capacity_held(&store, &first);
    }
}
