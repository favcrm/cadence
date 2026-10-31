// Independent CAD-1365 acceptance: historical schema-45 media compatibility.
//
// Included inside `seal::tests` (`include!`). Scope: the real
// `witness_commit` schema guard must accept an ACTUAL current schema
// database only when the three CAD-1365 media TABLES (introduced at schema
// 45) are present in sqlite_master, must keep accepting a genuine schema 40
// database and a genuine physical schema 44 database (the three media tables
// absent), and must keep refusing a forged schema 45 stamp on a physical 44
// database, a current database missing a media table, and a future schema
// above the supported bound with the exact existing message, leaving the sealed latch,
// owner_witness rows and business events unchanged. Schema-46 fixtures omit
// the CAD-1423 notification tables so the historical media-table cases retain
// their intended scope; bootstrap columns and transfer tables are likewise
// dropped so the stamped 46 preimage matches a real main46 store.
// Synthetic permits come from the existing test-only factory; no runtime
// authority is minted and no HTTP or native path is exercised.

const CAD1365_S41_MSG: &str = "owner witness database schema is unsupported";
const CAD1365_S41_CHALLENGE: &[u8] = b"cad1365-s41";
const CAD1365_S41_ATTEMPT: &str = "attempt-s41";

/// A physical schema 46 store: notification and transfer tables removed,
/// bootstrap columns removed, schema-50 image lineage removed, stamped 46.
/// The three CAD-1365 media tables stay present.
fn cad1365_s41_physical46_fixture(dir: &TempDir) -> (std::path::PathBuf, Store) {
    let (db, store) = open_legacy(dir);
    {
        let raw = Connection::open(&db).unwrap();
        raw.execute_batch(
            "DROP TABLE human_push_deliveries;
             DROP TABLE human_push_presence;
             DROP TABLE human_notification_intents;
             DROP TABLE human_push_devices;
             DROP TABLE human_room_mutes;
             DROP TABLE human_notification_reads;
             DROP TABLE human_notification_prefs;
             DROP TABLE wiki_transfer_entries;
             DROP TABLE wiki_transfer_jobs;
             DROP TABLE app_media_lineage;
             ALTER TABLE assistants DROP COLUMN bootstrap_state;
             ALTER TABLE assistants DROP COLUMN bootstrap_template_version;
             ALTER TABLE assistants DROP COLUMN bootstrap_revision;
             UPDATE schema_version SET version=46;",
        )
        .unwrap();
    }
    (db, store)
}

/// A genuine schema 40 fixture: the physical 46 store with the three
/// CAD-1365 media tables dropped in FK-safe order, stamped 40.
fn cad1365_s41_v40_fixture(dir: &TempDir) -> (std::path::PathBuf, Store) {
    let (db, store) = cad1365_s41_physical46_fixture(dir);
    {
        let raw = Connection::open(&db).unwrap();
        raw.execute_batch(
            "DROP TABLE app_media_imports;
             DROP TABLE app_media_import_requests;
             DROP TABLE app_media_asset_origins;
             DROP TABLE app_media_publish_requests;
             DROP TABLE app_media_assets;
             UPDATE schema_version SET version=40;",
        )
        .unwrap();
    }
    (db, store)
}

/// A physical schema 44 fixture: the physical 46 store with ONLY the three
/// CAD-1365 media tables dropped, stamped 44. The result must match a main
/// schema 44 database: no `app_media*` object remains.
fn cad1365_s45_v44_fixture(dir: &TempDir) -> (std::path::PathBuf, Store) {
    let (db, store) = cad1365_s41_physical46_fixture(dir);
    {
        let raw = Connection::open(&db).unwrap();
        raw.execute_batch(
            "DROP TABLE app_media_imports;
             DROP TABLE app_media_import_requests;
             DROP TABLE app_media_asset_origins;
             DROP TABLE app_media_publish_requests;
             DROP TABLE app_media_assets;
             UPDATE schema_version SET version=44;",
        )
        .unwrap();
        let media_objects: i64 = raw
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name LIKE 'app_media%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(media_objects, 0, "physical 44 must carry no media object");
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
fn cad1365_schema45_owner_witness_acceptance() {
    // 1. The actual current store is accepted and its schema is witnessed.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = open_legacy(&dir);
        let actual: i64 = store
            .conn()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(actual, crate::rollout::SCHEMA_VERSION);
        cad1365_s41_seal(&db, &store);
        cad1365_s41_witness(&db, &store)
            .expect("actual current schema with media tables must be accepted");
        let witnessed: i64 = store
            .conn()
            .query_row("SELECT db_schema FROM owner_witness", [], |r| r.get(0))
            .unwrap();
        assert_eq!(witnessed, actual, "witness must record the actual schema");
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

    // 3. Genuine physical schema 44 (media tables absent, stamped 44, no
    //    other 45 object difference) is accepted by the real guard.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = cad1365_s45_v44_fixture(&dir);
        cad1365_s41_seal(&db, &store);
        cad1365_s41_witness(&db, &store).expect("genuine schema 44 must be accepted");
        let witnessed: i64 = store
            .conn()
            .query_row("SELECT db_schema FROM owner_witness", [], |r| r.get(0))
            .unwrap();
        assert_eq!(witnessed, 44, "witness must record actual schema 44");
    }

    // 4. Forged schema 45 stamp on the physical 44 database (no media tables)
    //    refuses with the exact message; latch, witness rows and events are unchanged.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = cad1365_s45_v44_fixture(&dir);
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE schema_version SET version=45", [])
            .unwrap();
        cad1365_s41_seal(&db, &store);
        let before = cad1365_s41_snapshot(&store);
        assert_eq!(before.0 .0, 1, "fixture must be sealed before witness");
        assert_eq!(before.0 .1, 0, "fixture must not already have a witness");
        let err = cad1365_s41_witness(&db, &store).unwrap_err();
        assert!(
            matches!(&err, Error::Rejected(message) if message == CAD1365_S41_MSG),
            "forged schema 45 on physical 44 must fail at the actual guard, got: {err}"
        );
        assert_eq!(cad1365_s41_snapshot(&store), before, "refusal changed state");
    }

    // 5. Current schema with one media table missing refuses with the
    //    same exact message and no state change.
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
            "current schema missing a media table must fail at the actual guard, got: {err}"
        );
        assert_eq!(cad1365_s41_snapshot(&store), before, "refusal changed state");
    }

    // 6. Unknown future schema above the supported bound is
    //    refused with the same exact message.
    {
        let dir = TempDir::new().unwrap();
        let (db, store) = open_legacy(&dir);
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE schema_version SET version=?1", [PROTECTED_SCHEMA_MAX + 1])
            .unwrap();
        cad1365_s41_seal(&db, &store);
        let before = cad1365_s41_snapshot(&store);
        let err = cad1365_s41_witness(&db, &store).unwrap_err();
        assert!(
            matches!(&err, Error::Rejected(message) if message == CAD1365_S41_MSG),
            "future schema must fail at the actual guard, got: {err}"
        );
        assert_eq!(cad1365_s41_snapshot(&store), before, "refusal changed state");
    }
}
