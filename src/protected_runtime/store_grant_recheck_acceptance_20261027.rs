//! Independent CAD-1085 grant-continuity acceptance.
//!
//! Child of protected_runtime::store. Uses the real SQLite schema, actual
//! allocation/enrollment/consume transitions, and real challenge codec. No
//! authenticated owner/installer/runtime proof or launch permit is fabricated.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;
use serde_json::json;

use crate::daemon::supervisor_grant::{canonical_challenge_bytes, parse_challenge_bytes};

use super::{challenge_digest, AllocParams, Allocation, ConsumeOutcome, OwnerStore, SCHEMA_V1};

const WORKSPACE: &str = "abababababababababababababababab";
const PROFILE: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn memory_owner() -> OwnerStore {
    let conn = Connection::open_in_memory().expect("real isolated owner SQLite connection");
    conn.execute_batch(SCHEMA_V1)
        .expect("actual production owner schema");
    let store = OwnerStore {
        conn: Mutex::new(conn),
        path: PathBuf::from(":memory:"),
        workspace_id: WORKSPACE.to_string(),
        boot_id: "grant-recheck-first-boot".to_string(),
    };
    store.begin_boot(1, 1).expect("actual initial owner boot");
    store
}

fn allocate_challenge(store: &OwnerStore) -> (Allocation, String) {
    let allocation = store
        .allocate(AllocParams {
            workspace_id: WORKSPACE,
            agent_uid: "agt-11111111111111111111111111111111",
            session_binding: "grant-recheck-fixture-session",
            server_generation: 1,
            expected_owner_epoch: store.owner_epoch().expect("live owner epoch"),
            database_epoch: 1,
            profile_digest: PROFILE,
            max_runtimes: 1,
            uid_start: 21001,
            uid_end: 21999,
            run_dir: Path::new("/unused-cad1085-grant-memory/run"),
        })
        .expect("actual fresh allocating obligation");
    let image = format!(
        "registry.invalid/cadence-grant-fixture@sha256:{}",
        "a".repeat(64)
    );
    let document = json!({
        "launch": {
            "request": {
                "identity": {
                    "company": WORKSPACE,
                    "instance": allocation.runtime_id,
                    "backend": "native",
                    "tier": "basic",
                    "generation": allocation.generation
                },
                "purpose": "fresh_start",
                "challenge": allocation.operation_id,
                "image": image
            },
            "epoch": allocation.owner_epoch
        },
        "recipient": {
            "pid": 1234,
            "starttime": "987654321",
            "generation": "b".repeat(32),
            "nonce": "123e4567-e89b-42d3-a456-426614174012"
        },
        "pins": {
            "source": "c".repeat(40),
            "image": image,
            "helper": "d".repeat(64),
            "node": "e".repeat(64),
            "piGraph": "f".repeat(64),
            "policy": "0".repeat(64)
        },
        "lineage": {
            "reference": "grant-recheck-fixture",
            "databaseEpoch": 1
        }
    });
    let encoded = serde_json::to_vec(&document).expect("encode complete fixture challenge");
    let parsed = parse_challenge_bytes(&encoded)
        .expect("actual full challenge grammar accepts fixture metadata");
    let canonical = canonical_challenge_bytes(&parsed).expect("actual canonical challenge codec");
    let digest = challenge_digest(&canonical);
    let text = String::from_utf8(canonical).expect("canonical challenge is UTF-8");
    store
        .set_challenge(&allocation.runtime_id, &text, &digest)
        .expect("actual challenge binding while allocating");
    (allocation, digest)
}

fn read_only_recheck(store: &OwnerStore, digest: &str, epoch: u64) -> bool {
    let before = store
        .conn
        .lock()
        .expect("owner fixture connection")
        .total_changes();
    let current = store
        .recheck_grant(digest, epoch)
        .expect("actual read-only grant continuity query");
    let after = store
        .conn
        .lock()
        .expect("owner fixture connection")
        .total_changes();
    assert_eq!(before, after, "grant recheck must never mutate owner state");
    current
}

#[test]
fn cad1085_grant_recheck_preserves_consumed_continuity_without_replay() {
    for ending in ["fence", "restart", "start"] {
        let mut store = memory_owner();
        let (allocation, digest) = allocate_challenge(&store);
        let epoch = store.owner_epoch().expect("current owner epoch");
        let wrong_digest = challenge_digest(b"independent missing challenge");
        assert_ne!(digest, wrong_digest);

        assert!(!read_only_recheck(&store, &digest, epoch));
        assert!(!store
            .enrolled(&digest, epoch)
            .expect("allocating is not enrolled"));
        store
            .mark_prepared(&allocation.runtime_id, &digest)
            .expect("actual allocating to prepared transition");
        assert!(store.enrolled(&digest, epoch).expect("prepared enrollment"));
        assert!(read_only_recheck(&store, &digest, epoch));
        assert!(!read_only_recheck(&store, &wrong_digest, epoch));
        assert!(!read_only_recheck(&store, &digest, epoch + 1));
        assert_eq!(
            store
                .consume_by_challenge(&wrong_digest)
                .expect("wrong challenge consume outcome"),
            ConsumeOutcome::Unknown
        );
        assert!(read_only_recheck(&store, &digest, epoch));

        assert_eq!(
            store
                .consume_by_challenge(&digest)
                .expect("actual first durable consume"),
            ConsumeOutcome::Consumed
        );
        assert_eq!(
            store
                .get(&allocation.runtime_id)
                .expect("consumed row")
                .phase,
            "consumed"
        );
        assert!(
            read_only_recheck(&store, &digest, epoch),
            "the legitimate post-consume grant recheck must remain current"
        );
        assert!(
            !store
                .enrolled(&digest, epoch)
                .expect("query prepared-only enrollment"),
            "consumed continuity must not reenroll a consumed grant"
        );
        assert_eq!(
            store
                .consume_by_challenge(&digest)
                .expect("actual duplicate durable consume"),
            ConsumeOutcome::Unknown,
            "continuity must not recreate one-use consume authority"
        );
        assert!(read_only_recheck(&store, &digest, epoch));
        assert!(!read_only_recheck(&store, &wrong_digest, epoch));
        assert!(!read_only_recheck(&store, &digest, epoch + 1));

        match ending {
            "fence" => {
                store
                    .fence(
                        &allocation.runtime_id,
                        allocation.generation,
                        "independent grant continuity fence",
                    )
                    .expect("actual consumed to revoking transition");
                assert_eq!(
                    store.get(&allocation.runtime_id).expect("fenced row").phase,
                    "revoking"
                );
                assert!(!read_only_recheck(&store, &digest, epoch));
            }
            "restart" => {
                store.boot_id = "grant-recheck-second-boot".to_string();
                store
                    .begin_boot(1, 2)
                    .expect("actual owner restart transition");
                let current = store.owner_epoch().expect("new live owner epoch");
                assert!(current > epoch);
                assert!(!read_only_recheck(&store, &digest, epoch));
                assert!(!read_only_recheck(&store, &digest, current));
            }
            "start" => {
                store
                    .mark_starting(&allocation.runtime_id, allocation.generation)
                    .expect("actual consumed to starting transition");
                assert!(!read_only_recheck(&store, &digest, epoch));
                store
                    .mark_observing(
                        &allocation.runtime_id,
                        allocation.generation,
                        &[0x43; 32],
                        1234,
                        987654321,
                    )
                    .expect("actual starting to observing metadata transition");
                assert!(!read_only_recheck(&store, &digest, epoch));
            }
            _ => unreachable!(),
        }
        assert!(!store
            .enrolled(&digest, epoch)
            .expect("no terminal case returns to prepared enrollment"));
        assert_eq!(
            store
                .consume_by_challenge(&digest)
                .expect("consume after invalidation/start"),
            ConsumeOutcome::Unknown
        );
    }
}
