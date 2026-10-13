// Independent CAD-1365 acceptance: owner-witness schema-41 compatibility.
//
// Included inside `seal::tests` (`include!`). Scope: the real
// `witness_commit` schema guard must accept an ACTUAL schema 41 database
// only when the three CAD-1365 media TABLES are present in sqlite_master,
// must keep accepting a genuine schema 40 database, and must keep refusing
// a forged or unknown schema stamp with the exact existing message, leaving
// the sealed latch, owner_witness rows and business events unchanged.
// Synthetic permits come from the existing test-only factory; no runtime
// authority is minted and no HTTP or native path is exercised.

const CAD1365_S41_MSG: &str = "owner witness database schema is unsupported";
const CAD1365_S41_CHALLENGE: &[u8] = b"cad1365-s41";
const CAD1365_S41_ATTEMPT: &str = "attempt-s41";

/// A genuine schema 40 fixture: a fresh (migrated to 41) store with the
/// three CAD-1365 media tables dropped in FK-safe order, stamped 40.
fn cad1365_s41_v40_fixture(dir: &TempDir) -> (std::path::PathBuf, Store) {
    let (db, store) = open_legacy(dir);
    {
        let raw = Connection::open(&db).unwrap();
        raw.execute_batch(
            "DROP TABLE app_media_asset_origins;
             DROP TABLE app_media_publish_requests;
             DROP TABLE app_media_assets;
             UPDATE schema_version SET version=40;",
        )
        .unwrap();
    }
    (db, store)
}

type CAD1365S41Latch = (i64, i64, Vec<u8>, String, Option<String>, i64);

fn cad1365_s41_snapshot(store: &Store) -> (CAD1365S41Latch, i64, i64) {
    let conn = store.conn();
    let latch = conn
        .query_row(
            "SELECT closed,witness_done,challenge,attempt,artifact,epoch FROM closure_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    let witnesses: i64 = conn
        .query_row("SELECT count(*) FROM owner_witness", [], |r| r.get(0))
        .unwrap();
    let events: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    (latch, witnesses, events)
}

fn cad1365_s41_seal(db: &std::path::Path, store: &Store) {
    store
        .propose_close(
            &permit(
                db,
                OwnerOp::Close,
                CAD1365_S41_CHALLENGE,
                CAD1365_S41_ATTEMPT,
                "art",
                1,
            ),
            "test",
        )
        .unwrap();
}

fn cad1365_s41_witness(db: &std::path::Path, store: &Store) -> Result<u64> {
    store.witness_commit(&permit(
        db,
        OwnerOp::Witness,
        CAD1365_S41_CHALLENGE,
        CAD1365_S41_ATTEMPT,
        "art",
        1,
    ))
}

#[test]
fn cad1365_schema41_owner_witness_acceptance() {
    // 1. Actual fresh schema 41 (all three media tables present) is accepted
    //    and the witness records db_schema 41.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = open_legacy(&dir);
        let actual: i64 = store
            .conn()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(actual, 41, "fresh store must migrate to schema 41");
        cad1365_s41_seal(&db, &store);
        cad1365_s41_witness(&db, &store)
            .expect("actual schema 41 with media tables must be accepted");
        let witnessed: i64 = store
            .conn()
            .query_row("SELECT db_schema FROM owner_witness", [], |r| r.get(0))
            .unwrap();
        assert_eq!(witnessed, 41, "witness must record actual schema 41");
    }

    // 2. Genuine schema 40 (media tables absent, stamped 40) is still accepted.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = cad1365_s41_v40_fixture(&dir);
        cad1365_s41_seal(&db, &store);
        cad1365_s41_witness(&db, &store).expect("genuine schema 40 must be accepted");
        let witnessed: i64 = store
            .conn()
            .query_row("SELECT db_schema FROM owner_witness", [], |r| r.get(0))
            .unwrap();
        assert_eq!(witnessed, 40, "witness must record actual schema 40");
    }

    // 3. Forged schema 41 stamp on the v40 fixture (no media tables) refuses
    //    with the exact message; latch, witness rows and events are unchanged.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = cad1365_s41_v40_fixture(&dir);
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE schema_version SET version=41", [])
            .unwrap();
        cad1365_s41_seal(&db, &store);
        let before = cad1365_s41_snapshot(&store);
        assert_eq!(before.0 .0, 1, "fixture must be sealed before witness");
        assert_eq!(before.0 .1, 0, "fixture must not already have a witness");
        let err = cad1365_s41_witness(&db, &store).unwrap_err();
        assert!(
            matches!(&err, Error::Rejected(message) if message == CAD1365_S41_MSG),
            "forged schema 41 on v40 must fail at the actual guard, got: {err}"
        );
        assert_eq!(cad1365_s41_snapshot(&store), before, "refusal changed state");
    }

    // 4. Real fresh schema 41 with one media table missing (emptied fixture)
    //    refuses with the same exact message and no state change.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = open_legacy(&dir);
        Connection::open(&db)
            .unwrap()
            .execute_batch("DROP TABLE app_media_publish_requests;")
            .unwrap();
        cad1365_s41_seal(&db, &store);
        let before = cad1365_s41_snapshot(&store);
        let err = cad1365_s41_witness(&db, &store).unwrap_err();
        assert!(
            matches!(&err, Error::Rejected(message) if message == CAD1365_S41_MSG),
            "schema 41 missing a media table must fail at the actual guard, got: {err}"
        );
        assert_eq!(cad1365_s41_snapshot(&store), before, "refusal changed state");
    }

    // 5. Unknown schema 42 is refused with the same exact message.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = open_legacy(&dir);
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE schema_version SET version=42", [])
            .unwrap();
        cad1365_s41_seal(&db, &store);
        let before = cad1365_s41_snapshot(&store);
        let err = cad1365_s41_witness(&db, &store).unwrap_err();
        assert!(
            matches!(&err, Error::Rejected(message) if message == CAD1365_S41_MSG),
            "unknown schema 42 must fail at the actual guard, got: {err}"
        );
        assert_eq!(cad1365_s41_snapshot(&store), before, "refusal changed state");
    }
}
