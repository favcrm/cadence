//! CAD-1161 independently authored bad-case guard (aos159-constructor-guard).
//! Implementers must not edit/weaken this file. Exercises the actual production
//! in-transaction incarnation and fixed-path guards with real SQLite, not a
//! client-side mirror. Fixed-path refusal creates no authority or service peer.
//! Binding is a public selector, NEVER a forged positive StoreOwnerGrant.
//! Grant issuance/one-use replay and protected-open/close/witness integration
//! remain to be exercised once the constructor's real relay contract exists.
use super::owner;
use rusqlite::Connection;
#[cfg(test)]
use rusqlite::{params, TransactionBehavior};
use std::cell::Cell;
use std::path::Path;

thread_local! {
    static SQLITE_OPENS: Cell<u64> = const { Cell::new(0) };
}

// Observe real SQLite opens, never replace/deny them or inject Store authority.
// Per-thread counts exclude unrelated CI tests. The real entrypoints below are
// synchronous; no product hook, trusted boolean or caller signature is added.
unsafe extern "C" fn observe_sqlite_open(
    _db: *mut rusqlite::ffi::sqlite3,
    _error: *mut *mut std::ffi::c_char,
    _api: *const rusqlite::ffi::sqlite3_api_routines,
) -> i32 {
    let _ = SQLITE_OPENS.try_with(|count| count.set(count.get().saturating_add(1)));
    rusqlite::ffi::SQLITE_OK
}

struct SqliteOpenProbe;
impl SqliteOpenProbe {
    fn new() -> Self {
        // SAFETY: static C-ABI function, no borrowed callback state or pointer
        // dereferences. It only observes; SQLITE_OK preserves normal opening.
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_auto_extension(Some(observe_sqlite_open)) },
            rusqlite::ffi::SQLITE_OK
        );
        Self
    }
    fn count(&self) -> u64 {
        SQLITE_OPENS.with(Cell::get)
    }
}
impl Drop for SqliteOpenProbe {
    fn drop(&mut self) {
        // SAFETY: unregister ONLY our static callback, even on assertion panic;
        // never reset/delete somebody else's registered SQLite extensions.
        unsafe { rusqlite::ffi::sqlite3_cancel_auto_extension(Some(observe_sqlite_open)) };
    }
}

#[cfg(test)]
fn enclave_refused<T>(result: crate::error::Result<T>) {
    match result {
        Err(crate::Error::Rejected(message)) => assert_eq!(
            message,
            "raw/legacy writer refused in the protected Store enclave"
        ),
        _ => panic!("raw/Legacy opening did not reach the real enclave refusal"),
    }
}

#[cfg(test)]
fn alias_refused<T>(result: crate::error::Result<T>) {
    match result {
        Err(crate::Error::Rejected(message)) => {
            assert_eq!(message, "unresolved raw/legacy writer alias refused");
        }
        _ => panic!("unresolved alias did not reach the real fail-closed guard"),
    }
}

#[cfg(test)]
fn database_files(path: &Path) -> Vec<Option<Vec<u8>>> {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .map(
            |suffix| match std::fs::read(format!("{}{suffix}", path.display())) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("could not inspect isolated database: {error}"),
            },
        )
        .collect()
}

#[cfg(test)]
fn refused_without_mutation(conn: &mut Connection, path: &Path, binding: &owner::Binding) {
    // Exclude malformed selector/timeout false positives: this candidate is
    // valid public syntax and must reach the real database identity comparison.
    binding.validate().unwrap();
    let files = database_files(path);
    let before: (String, String, i64) = conn
        .query_row(
            "SELECT database_id,incarnation,epoch FROM store_incarnation WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    match owner::check_identity(&tx, binding, false) {
        Err(crate::Error::Rejected(message)) => assert_eq!(
            message,
            "Store owner permit is bound to a different database incarnation/epoch"
        ),
        _ => panic!("wrong/replaced incarnation did not reach the real refusal"),
    }
    let after: (String, String, i64) = tx
        .query_row(
            "SELECT database_id,incarnation,epoch FROM store_incarnation WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(after, before, "refused selector changed durable identity");
    assert_eq!(
        tx.query_row("SELECT value FROM canary WHERE id=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        159,
        "refusal changed business state"
    );
    tx.rollback().unwrap();
    assert_eq!(
        database_files(path),
        files,
        "incarnation refusal mutated the database or SQLite sidefiles"
    );
}

/// Independently authored LOCAL retained-opening replay, not remote CAS,
/// first-open admission or late-current acceptance. Root alone supplies the
/// genuinely opened Shared Store and Prepared/Begin/Checked/Stop FD3 window.
/// Pre-Begin original custody/current and post-Checked current stay OUTSIDE
/// this comparison. No caller selectors, grants, callbacks or authority output.
#[cfg(all(
    debug_assertions,
    feature = "test-seam",
    target_os = "linux",
    target_arch = "x86_64"
))]
pub(crate) fn native_retained_opening_replay(store: &crate::store::Store) -> crate::Result<()> {
    use std::fs::{Metadata, OpenOptions};
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::PathBuf;

    #[derive(Debug, PartialEq, Eq)]
    struct FileState {
        device: u64,
        inode: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        links: u64,
        length: u64,
        accessed: (i64, i64),
        modified: (i64, i64),
        changed: (i64, i64),
        bytes: Option<Vec<u8>>,
        link: Option<PathBuf>,
    }
    fn state(metadata: &Metadata) -> FileState {
        FileState {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            length: metadata.len(),
            accessed: (metadata.atime(), metadata.atime_nsec()),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            bytes: None,
            link: None,
        }
    }
    fn files(path: &Path) -> crate::Result<Vec<Option<FileState>>> {
        ["", "-wal", "-shm", "-journal"]
            .into_iter()
            .map(|suffix| {
                let mut name = path.as_os_str().to_os_string();
                name.push(suffix);
                let name = PathBuf::from(name);
                let metadata = match std::fs::symlink_metadata(&name) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.into()),
                };
                let mut snapshot = state(&metadata);
                if metadata.file_type().is_symlink() {
                    snapshot.link = Some(std::fs::read_link(&name)?);
                } else if metadata.is_file() {
                    // Read only our actual daemon-owned file, without following
                    // a replacement link or causing snapshot atime writes. No
                    // privilege acquisition/fallback if O_NOATIME is refused.
                    let file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NOATIME)
                        .open(&name)?;
                    if state(&file.metadata()?) != snapshot {
                        return Err(crate::Error::rejected(
                            "retained replay snapshot inode changed",
                        ));
                    }
                    let mut bytes = Vec::new();
                    (&file).read_to_end(&mut bytes)?;
                    if state(&file.metadata()?) != snapshot
                        || state(&std::fs::symlink_metadata(&name)?) != snapshot
                    {
                        return Err(crate::Error::rejected(
                            "retained replay snapshot changed while read",
                        ));
                    }
                    snapshot.bytes = Some(bytes);
                }
                Ok(Some(snapshot))
            })
            .collect()
    }
    #[derive(Debug, PartialEq, Eq)]
    struct Canary {
        identity: (String, String, i64, String, String),
        highwater: i64,
        latch: (i64, i64, i64),
        changes: i64,
    }
    fn canary(conn: &Connection) -> crate::Result<Canary> {
        Ok(Canary {
            identity: conn.query_row(
                "SELECT database_id,incarnation,epoch,operation,artifact FROM store_incarnation WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )?,
            highwater: conn.query_row(
                "SELECT sequence FROM store_business_highwater WHERE id=1", [], |row| row.get(0),
            )?,
            latch: conn.query_row(
                "SELECT closed,witness_done,epoch FROM closure_state WHERE id=1", [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?,
            changes: conn.query_row("SELECT total_changes()", [], |row| row.get(0))?,
        })
    }

    if store.db_identity != owner::DATABASE_PATH
        || !store.protected_open
        || store.conn.is_poisoned()
    {
        return Err(crate::Error::rejected(
            "retained replay requires the healthy original protected Store",
        ));
    }
    // Use the SAME existing connection, disarmed for writes. Reject poison
    // before conn() so its recovery/forensic writer is not a probe effect.
    // Root must independently exclude other writers/readbacks in this window;
    // this lock is not a caller-supplied quiescence or admission certificate.
    let conn = store.conn();
    let before_canary = canary(&conn)?;
    let path = Path::new(owner::DATABASE_PATH);
    let before_files = files(path)?;
    if before_files[0]
        .as_ref()
        .is_none_or(|file| file.bytes.is_none() || file.link.is_some())
    {
        return Err(crate::Error::rejected(
            "retained replay original database is absent or not regular",
        ));
    }
    let sql_opens = SqliteOpenProbe::new();
    let before_opens = sql_opens.count();
    assert!(before_opens < u64::MAX, "SQLite open observer exhausted");

    match store.reconsume_owned_opening() {
        Err(crate::Error::Rejected(message)) => assert_eq!(
            message, "Store owner permit spent or UNKNOWN; replay refused",
            "retained original consume failed at a different boundary"
        ),
        _ => panic!("retained original opening consume did not reach exact LOCAL spent refusal"),
    }
    assert_eq!(
        sql_opens.count(),
        before_opens,
        "retained consume opened SQLite"
    );
    // No current/flush/checkpoint/recovery or newly opened SQLite reader here.
    // These actual read-only canaries use the already-held original connection.
    assert_eq!(
        canary(&conn)?,
        before_canary,
        "retained replay changed actual identity/latch/business canary"
    );
    assert_eq!(
        files(path)?,
        before_files,
        "retained replay changed database/sidefile bytes, links, metadata or absence"
    );
    assert_eq!(
        sql_opens.count(),
        before_opens,
        "retained comparison opened SQLite"
    );
    // All comparisons and observer/connection release finish before caller's
    // Checked. Root performs post-current only afterward, then owned Stop/reap;
    // this diagnostic must never continue into serving or business writers.
    Ok(())
}

#[cfg(test)]
#[test]
fn store_owner_wrong_or_replaced_incarnation_refuses_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cad1161.sqlite3");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch(owner::IDENTITY_SCHEMA).unwrap();
    conn.execute_batch(
        "CREATE TABLE canary(id INTEGER PRIMARY KEY,value INTEGER NOT NULL);
         INSERT INTO canary VALUES(1,159);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO store_incarnation VALUES(1,?1,?2,?3,?4,?5)",
        params![
            "cad1161-db",
            "incarnation-one",
            1,
            "open-one",
            "artifact-one"
        ],
    )
    .unwrap();
    let binding = owner::Binding {
        database_id: "cad1161-db".into(),
        incarnation: "incarnation-one".into(),
        database_epoch: 1,
        operation: "open-one".into(),
        purpose: owner::Purpose::Open,
        path: path.to_str().unwrap().into(),
        challenge: vec![0x61; 32],
        attempt: "attempt-one".into(),
        artifact: "artifact-one".into(),
        deadline_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 60,
        source: None,
    };
    binding.validate().unwrap();
    // A shape-valid, matching local row reaches this real comparison. This
    // read-only success does NOT mint an opening/maintenance/restore permit.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    owner::check_identity(&tx, &binding, false).unwrap();
    tx.rollback().unwrap();

    // The matching local identity above remains a valid pure comparison, but
    // neither an existing alternate database nor an absent caller-chosen file
    // may be elected as the fixed protected database. These absolute paths are
    // entirely in our isolated directory: never inspect the real enclave.
    let absent_path = dir.path().join("must-not-be-created.sqlite3");
    let files_before_path_refusal = database_files(&path);
    for requested in [path.as_path(), absent_path.as_path()] {
        assert!(requested.is_absolute());
        assert_ne!(requested, Path::new(owner::DATABASE_PATH));
        let candidate_files = database_files(requested);
        match owner::check_path(requested) {
            Err(crate::Error::Rejected(message)) => assert_eq!(
                message, "Store owner path is not the exact canonical regular database",
                "alternate path did not reach the actual fixed-path refusal"
            ),
            _ => panic!("caller elected an alternate protected database path"),
        }
        assert_eq!(
            database_files(requested),
            candidate_files,
            "path refusal created or changed the candidate database/sidefiles"
        );
        assert_eq!(
            database_files(&path),
            files_before_path_refusal,
            "path refusal changed the existing database/sidefiles"
        );
    }
    assert_eq!(
        conn.query_row("SELECT value FROM canary WHERE id=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        159,
        "path refusal changed business state"
    );

    // Establish a real allowed outside-enclave baseline, including the public
    // Legacy constructor, so unconditional refusal cannot masquerade as PASS.
    // The observer must see actual SQLite opens before we trust its zero count.
    let sql_opens = SqliteOpenProbe::new();
    let before_raw_baseline = sql_opens.count();
    super::preflight_writer_guard(&path).unwrap();
    assert!(sql_opens.count() > before_raw_baseline);
    let legacy_path = dir.path().join("allowed-legacy.sqlite3");
    let before_legacy_baseline = sql_opens.count();
    let legacy = super::Store::open(&legacy_path).unwrap();
    assert!(sql_opens.count() > before_legacy_baseline);
    drop(legacy);
    let identity_files = database_files(&path);
    let legacy_files = database_files(&legacy_path);

    // These are reserved path SELECTORS, not seeded/live enclave fixtures.
    // Never read/create an artifact in the real enclave. Actual raw preflight
    // and public Store::open must return their specific policy refusal without
    // opening SQLite, including the shadow cadence.sqlite3 spelling and child.
    let fixed = Path::new(owner::DATABASE_PATH);
    let enclave = fixed.parent().unwrap();
    for requested in [
        fixed.to_path_buf(),
        enclave.join("cadence.sqlite3"),
        enclave.join("unprovisioned").join("shadow.sqlite3"),
    ] {
        let before_refusal = sql_opens.count();
        enclave_refused(super::preflight_writer_guard(&requested));
        assert_eq!(
            sql_opens.count(),
            before_refusal,
            "raw preflight opened SQLite before refusing the enclave"
        );
        enclave_refused(super::Store::open(&requested));
        assert_eq!(
            sql_opens.count(),
            before_refusal,
            "Legacy Store::open opened SQLite before refusing the enclave"
        );
        assert_eq!(database_files(&path), identity_files);
        assert_eq!(database_files(&legacy_path), legacy_files);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        {
            use std::fs::{File, OpenOptions};
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

            // CORE identity only: ordinary-UID real files, not Root creation,
            // UID21000 custody, an Init grant or an authenticated FD exchange.
            // Setup creation precedes the probe and is not a refusal effect.
            let init_path = dir.path().join("init-custody.sqlite3");
            let wrong_path = dir.path().join("wrong-init-custody.sqlite3");
            let create_file = |candidate: &Path| {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(candidate)
                    .unwrap()
            };
            let held = create_file(&init_path);
            let wrong = create_file(&wrong_path);
            let named = File::open(&init_path).unwrap();
            let held_stat = held.metadata().unwrap();
            let wrong_stat = wrong.metadata().unwrap();
            assert!(held_stat.is_file() && wrong_stat.is_file());
            assert_eq!(held_stat.len(), 0);
            assert_eq!(wrong_stat.len(), 0);
            assert_eq!(held_stat.nlink(), 1);
            assert_eq!(wrong_stat.nlink(), 1);
            assert_eq!(
                (
                    held_stat.dev(),
                    held_stat.uid(),
                    held_stat.gid(),
                    held_stat.mode(),
                ),
                (
                    wrong_stat.dev(),
                    wrong_stat.uid(),
                    wrong_stat.gid(),
                    wrong_stat.mode(),
                )
            );
            assert_ne!(held_stat.ino(), wrong_stat.ino());

            // Record bytes, links, held-name identity and genuine sidefile
            // absence BEFORE either comparison; never manufacture SQLite data.
            let links = |candidate: &Path| {
                ["", "-wal", "-shm", "-journal"]
                    .into_iter()
                    .map(|suffix| {
                        let name = format!("{}{suffix}", candidate.display());
                        match std::fs::symlink_metadata(&name) {
                            Ok(metadata) => {
                                let link = metadata
                                    .file_type()
                                    .is_symlink()
                                    .then(|| std::fs::read_link(&name).unwrap());
                                Some((
                                    metadata.dev(),
                                    metadata.ino(),
                                    metadata.uid(),
                                    metadata.gid(),
                                    metadata.mode(),
                                    metadata.nlink(),
                                    metadata.len(),
                                    link,
                                ))
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                            Err(error) => panic!("could not inspect Init probe files: {error}"),
                        }
                    })
                    .collect::<Vec<_>>()
            };
            let observed = [
                init_path.as_path(),
                wrong_path.as_path(),
                path.as_path(),
                legacy_path.as_path(),
                absent_path.as_path(),
            ];
            let before_files: Vec<_> = observed
                .iter()
                .map(|candidate| (database_files(candidate), links(candidate)))
                .collect();
            for candidate in [&init_path, &wrong_path] {
                let files = database_files(candidate);
                assert_eq!(files[0].as_deref(), Some(&[][..]));
                assert!(files[1..].iter().all(Option::is_none));
            }
            let before_opens = sql_opens.count();
            owner::check_init_file_identity(&held, &named).unwrap();
            // Both inputs are regular, empty and otherwise matching metadata.
            // Direct SAME production predicate reaches inode comparison, not
            // an earlier fixed-path/UID/deadline/phase/cold-service rejection.
            match owner::check_init_file_identity(&wrong, &named) {
                Err(crate::Error::Rejected(message)) => {
                    assert_eq!(message, "Store Init file descriptor/name identity changed")
                }
                _ => panic!("different real Init FD reached identity success"),
            }
            assert_eq!(
                sql_opens.count(),
                before_opens,
                "Init identity opened SQLite"
            );
            assert_eq!(
                observed
                    .iter()
                    .map(|candidate| (database_files(candidate), links(candidate)))
                    .collect::<Vec<_>>(),
                before_files,
                "Init identity comparison mutated bytes, links or four-file absence"
            );
        }
        let healthy_alias = dir.path().join("healthy-alias.sqlite3");
        symlink(&path, &healthy_alias).unwrap();
        let before_healthy_alias = sql_opens.count();
        super::preflight_writer_guard(&healthy_alias).unwrap();
        assert!(
            sql_opens.count() > before_healthy_alias,
            "supported healthy alias did not reach real SQLite"
        );

        // A dangling leaf or chain must not elect a fresh database. Every
        // symlink and target lives in our temp directory, never the enclave.
        let dangling_alias = dir.path().join("dangling-alias.sqlite3");
        let chained_alias = dir.path().join("chained-alias.sqlite3");
        symlink(&absent_path, &dangling_alias).unwrap();
        symlink(&dangling_alias, &chained_alias).unwrap();
        let absent_files = database_files(&absent_path);
        assert!(absent_files.iter().all(Option::is_none));
        for requested in [&dangling_alias, &chained_alias] {
            let link_before = std::fs::read_link(requested).unwrap();
            let candidate_files = database_files(requested);
            let before_refusal = sql_opens.count();
            alias_refused(super::preflight_writer_guard(requested));
            assert_eq!(sql_opens.count(), before_refusal);
            alias_refused(super::Store::open(requested));
            assert_eq!(
                sql_opens.count(),
                before_refusal,
                "unresolved alias reached SQLite before refusal"
            );
            assert_eq!(std::fs::read_link(requested).unwrap(), link_before);
            assert_eq!(database_files(requested), candidate_files);
            assert_eq!(database_files(&absent_path), absent_files);
            assert_eq!(database_files(&path), identity_files);
            assert_eq!(database_files(&legacy_path), legacy_files);
        }
    }
    drop(sql_opens);

    let mut wrong_database = binding.clone();
    wrong_database.database_id = "different-database".into();
    refused_without_mutation(&mut conn, &path, &wrong_database);
    let mut wrong_epoch = binding.clone();
    wrong_epoch.database_epoch = 2;
    refused_without_mutation(&mut conn, &path, &wrong_epoch);

    // Advance ONLY isolated setup state. Reusing the old incarnation binding
    // now refuses despite the same database/path; this is stale-incarnation
    // refusal, NOT proof of an opaque consumed permit's same-epoch replay gate.
    conn.execute(
        "UPDATE store_incarnation SET incarnation='incarnation-two',epoch=2,
         operation='open-two',artifact='artifact-two' WHERE id=1",
        [],
    )
    .unwrap();
    refused_without_mutation(&mut conn, &path, &binding);
}
