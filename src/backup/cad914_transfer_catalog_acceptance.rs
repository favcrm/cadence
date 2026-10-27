//! CAD-914 independent acceptance: portable coordination export must not
//! transplant resumable wiki-transfer custody, and must still fail closed on
//! unrecognized schema. Author: independent acceptance (not the implementer).
//! Written from the ticket and the portable catalog contract, not from the
//! catalog source. Do not weaken it.
//!
//! Transfer jobs, received bytes, outcomes, owner keys, admitted revisions,
//! manifests and staged bytes are transient custody. A current-schema store
//! that holds a populated job and entry must export, document that those
//! tables were omitted, and leave the live records unchanged. The exported
//! package and a real inactive restore of it must contain no resumable
//! transfer rows. Content SHA-256 is not the admitted revision pin.
//! An unknown column on a known transfer table, and a future
//! `wiki_transfer_future` table created only in the fresh fixture, must
//! refuse before any output exists and must leave the source unchanged. The
//! future name shares the `wiki_transfer_` prefix, so a prefix admission
//! cannot pass. Fixtures live under a fresh owned temp dir and are removed
//! with it. No activation and no provider.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::backup::portable::{
    export_portable, restore_inactive, PortableExportOptions, PortableRestoreOptions, Scope,
};
use crate::rollout::db_file;
use crate::store::Store;

const JOB: &str = "cad914-job-synthetic";
const ENTRY: &str = "cad914-entry-synthetic";
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OWNER: &str = "wk:cad914-synthetic-owner";
const ROOT: &str = "wiki/cad914-synthetic";
const MANIFEST: &str =
    r#"{"version":1,"source_id":"cad914-src","root":"wiki/cad914-synthetic","entries":[]}"#;
const RECEIVED: i64 = 4096;
const OUTCOME: &str = "unresolved";
const ROOT_ATTEMPT: &str = "unresolved";
/// Content hash of the staged bytes. Not a destination revision.
const CONTENT_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// Admitted destination revision pin (`if_rev`), distinct from the content hash.
const ADMITTED_REV: &str = "fnv1a:c0ad9140ad0717ed";
/// Recorded publication revision, distinct from both the pin and the content hash.
const RECORDED_REV: &str = "fnv1a:c0ad9140f1a17ed1";

const TRANSFER_TABLES: &[&str] = &["wiki_transfer_jobs", "wiki_transfer_entries"];

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "cad914-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad914-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn make_tracker(root: &Path) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(tracker.join("tickets")).unwrap();
    git(&tracker, &["init", "-q"]);
    fs::write(
        tracker.join("tickets/CAD-914.md"),
        "# CAD-914 synthetic ticket fixture\n",
    )
    .unwrap();
    git(&tracker, &["add", "-A"]);
    git(&tracker, &["commit", "-q", "-m", "CAD-914 fixture"]);
    tracker
}

fn make_state(root: &Path) -> PathBuf {
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    drop(Store::open(&db_file(&state)).unwrap());
    state
}

fn opts(tracker: &Path) -> PortableExportOptions {
    PortableExportOptions {
        scope: Scope::Coordination,
        tracker: tracker.to_path_buf(),
        redact_history_secrets: false,
    }
}

fn no_partial(out: &Path) -> bool {
    let parent = out.parent().unwrap();
    let name = out.file_name().unwrap().to_string_lossy().into_owned();
    let prefix = format!(".{name}.partial-");
    fs::read_dir(parent)
        .unwrap()
        .flatten()
        .all(|entry| !entry.file_name().to_string_lossy().starts_with(&prefix))
}

fn count(db: &Path, table: &str) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn table_exists(db: &Path, table: &str) -> bool {
    Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        > 0
}

/// One synthetic job and one synthetic entry covering owner, root, manifest,
/// digest, admitted `if_rev`, received bytes, outcome and content hash.
/// Written only into the fresh temp store.
fn seed_transfer(state: &Path) {
    let conn = Connection::open(db_file(state)).unwrap();
    conn.execute(
        "INSERT INTO wiki_transfer_jobs(job_id,digest,manifest_json,source_id,root,owner_key,state,root_attempt,index_kicked,created_at)
         VALUES(?1,?2,?3,'cad914-src',?4,?5,'open',?6,0,1)",
        params![JOB, DIGEST, MANIFEST, ROOT, OWNER, ROOT_ATTEMPT],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO wiki_transfer_entries(job_id,entry_id,ordinal,path,rel_path,kind,size,sha256,if_rev,received,outcome,rev,extraction,extraction_rev,source_hash,detail)
         VALUES(?1,?2,0,?3,'note.md','text',?4,?5,?6,?4,?7,?8,'not_applicable',NULL,?5,'synthetic')",
        params![
            JOB,
            ENTRY,
            format!("{ROOT}/note.md"),
            RECEIVED,
            CONTENT_SHA,
            ADMITTED_REV,
            OUTCOME,
            RECORDED_REV
        ],
    )
    .unwrap();
}

fn assert_source_custody(state: &Path) {
    let db = db_file(state);
    assert_eq!(count(&db, "wiki_transfer_jobs"), 1);
    assert_eq!(count(&db, "wiki_transfer_entries"), 1);
    let conn = Connection::open(&db).unwrap();
    let (digest, owner, root, manifest, attempt): (String, String, String, String, String) = conn
        .query_row(
            "SELECT digest, owner_key, root, manifest_json, root_attempt FROM wiki_transfer_jobs WHERE job_id=?1",
            [JOB],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(digest, DIGEST);
    assert_eq!(owner, OWNER);
    assert_eq!(root, ROOT);
    assert_eq!(manifest, MANIFEST);
    assert_eq!(attempt, ROOT_ATTEMPT);
    let (received, outcome, content_sha, if_rev, recorded_rev, source_hash): (
        i64,
        String,
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT received, outcome, sha256, if_rev, rev, source_hash FROM wiki_transfer_entries WHERE job_id=?1 AND entry_id=?2",
            params![JOB, ENTRY],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(received, RECEIVED);
    assert_eq!(outcome, OUTCOME);
    assert_eq!(content_sha, CONTENT_SHA);
    assert_eq!(if_rev, ADMITTED_REV);
    assert_eq!(recorded_rev, RECORDED_REV);
    assert_eq!(source_hash, CONTENT_SHA);
    assert_ne!(content_sha, if_rev);
}

const FUTURE_TABLE: &str = "wiki_transfer_future";

fn omitted_transfer_tables(manifest: &Value) -> bool {
    let Some(listed) = manifest["omitted_tables"].as_array() else {
        return false;
    };
    TRANSFER_TABLES
        .iter()
        .all(|table| listed.iter().any(|value| value.as_str() == Some(table)))
}

fn package_has_no_transfer_rows(package_db: &Path) -> bool {
    TRANSFER_TABLES
        .iter()
        .all(|table| !table_exists(package_db, table) || count(package_db, table) == 0)
}

#[test]
fn cad914_export_omits_populated_transfer_custody_and_restore_cannot_resume_it() {
    let owned = tempfile::Builder::new()
        .prefix("cad914-transfer-catalog-")
        .tempdir()
        .unwrap();
    let root = owned.path().join("omit");
    fs::create_dir_all(&root).unwrap();
    let tracker = make_tracker(&root);
    let state = make_state(&root);
    seed_transfer(&state);
    let source_bytes = fs::read(db_file(&state)).unwrap();

    let package = root.join("package");
    let report = export_portable(&state, &package, &opts(&tracker))
        .expect("known current schema with transfer rows exports");
    assert!(package.is_dir() && no_partial(&package));
    assert_eq!(report["full_workspace"], false);

    let manifest: Value =
        serde_json::from_slice(&fs::read(package.join("manifest.json")).unwrap()).unwrap();
    assert!(
        omitted_transfer_tables(&manifest),
        "transfer tables are documented as omitted, not silently selected"
    );
    let removed: Vec<&str> = manifest["omitted_rows"]
        .as_array()
        .expect("omitted row counts")
        .iter()
        .filter(|row| row[1].as_i64() == Some(1))
        .filter_map(|row| row[0].as_str())
        .collect();
    assert!(
        TRANSFER_TABLES.iter().all(|table| removed.contains(table)),
        "each populated transfer table records one omitted row"
    );
    assert!(
        package_has_no_transfer_rows(&package.join("cadence.sqlite3")),
        "package store carries no resumable transfer rows"
    );
    let packaged = fs::read(package.join("cadence.sqlite3")).unwrap();
    for marker in [
        JOB,
        OWNER,
        MANIFEST,
        DIGEST,
        ADMITTED_REV,
        CONTENT_SHA,
        RECORDED_REV,
    ] {
        assert!(
            !packaged
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()),
            "package store still contains transfer custody"
        );
    }

    assert_eq!(fs::read(db_file(&state)).unwrap(), source_bytes);
    assert_source_custody(&state);

    let restored = root.join("restored");
    let report = restore_inactive(
        &package,
        &restored,
        &PortableRestoreOptions { replace: false },
    )
    .expect("inactive restore of an omitted-custody package");
    assert_eq!(report["state"], "committed_inactive");
    assert!(
        package_has_no_transfer_rows(&restored.join("cadence.sqlite3")),
        "restored store cannot resume the source transfer"
    );
    assert_eq!(fs::read(db_file(&state)).unwrap(), source_bytes);
    assert!(restored.join("portable-restore.json").is_file());
}

#[test]
fn cad914_export_refuses_unknown_transfer_column_before_output() {
    let owned = tempfile::Builder::new()
        .prefix("cad914-transfer-catalog-")
        .tempdir()
        .unwrap();
    let root = owned.path().join("unknown-column");
    fs::create_dir_all(&root).unwrap();
    let tracker = make_tracker(&root);
    let state = make_state(&root);
    seed_transfer(&state);
    {
        let conn = Connection::open(db_file(&state)).unwrap();
        conn.execute_batch(
            "ALTER TABLE wiki_transfer_jobs ADD COLUMN staged_custody TEXT;
             UPDATE wiki_transfer_jobs SET staged_custody='cad914-forged-stage';",
        )
        .unwrap();
    }
    let before = fs::read(db_file(&state)).unwrap();

    let out = root.join("package-bad");
    let err = export_portable(&state, &out, &opts(&tracker))
        .expect_err("unknown transfer column refuses");
    assert_eq!(err.kind(), "rejected");
    assert!(!out.exists() && no_partial(&out));
    assert_eq!(fs::read(db_file(&state)).unwrap(), before);
    assert_source_custody(&state);
    let staged: i64 = Connection::open(db_file(&state))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM pragma_table_info('wiki_transfer_jobs') WHERE name='staged_custody'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(staged, 1, "refused column stays on the source");
}

#[test]
fn cad914_export_refuses_unknown_table_before_output() {
    let owned = tempfile::Builder::new()
        .prefix("cad914-transfer-catalog-")
        .tempdir()
        .unwrap();
    let root = owned.path().join("unknown-table");
    fs::create_dir_all(&root).unwrap();
    let tracker = make_tracker(&root);
    let state = make_state(&root);
    seed_transfer(&state);
    {
        let conn = Connection::open(db_file(&state)).unwrap();
        conn.execute(
            &format!("CREATE TABLE {FUTURE_TABLE}(id INTEGER PRIMARY KEY, payload TEXT)"),
            [],
        )
        .unwrap();
        conn.execute(
            &format!("INSERT INTO {FUTURE_TABLE}(payload) VALUES('cad914-future-table')"),
            [],
        )
        .unwrap();
    }
    let before = fs::read(db_file(&state)).unwrap();

    let out = root.join("package-unknown");
    let err = export_portable(&state, &out, &opts(&tracker))
        .expect_err("future wiki_transfer_* table refuses");
    assert_eq!(err.kind(), "rejected");
    assert!(!out.exists() && no_partial(&out));
    assert_eq!(fs::read(db_file(&state)).unwrap(), before);
    assert_source_custody(&state);
    assert!(table_exists(&db_file(&state), FUTURE_TABLE));
}
