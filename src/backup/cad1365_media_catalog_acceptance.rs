//! CAD-1365 independent acceptance: the portable export's closed catalog
//! must admit the three known media tables and refuse an unknown media
//! column. Author: independent acceptance (not the implementer). Written
//! from the ticket and the portable catalog contract, not from the
//! catalog source. Do not weaken it.
//!
//! One test, real product entrypoint: `export_portable` runs the real
//! `check_catalog` guard on a private snapshot of a real current (v45)
//! store. Positive control: the untouched store exports. Bad case: the
//! same store with one extra column on `app_media_assets` must refuse
//! with the exact catalog error, write no output directory and leave the
//! live store's schema version and the locator column still present.
//! Fixtures live under a fresh owned temp dir (TMPDIR), removed at test end.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;

use crate::backup::portable::{export_portable, PortableExportOptions, Scope};
use crate::rollout::db_file;
use crate::store::Store;

const CATALOG_REFUSAL: &str =
    "portable export refuses: table app_media_assets columns do not match the known catalog";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "cad1365-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad1365-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A clean tracker: one commit holding one ticket file.
fn make_tracker(root: &Path) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(tracker.join("tickets")).unwrap();
    git(&tracker, &["init", "-q"]);
    fs::write(
        tracker.join("tickets/CAD-1365.md"),
        "# CAD-1365 synthetic ticket fixture\n",
    )
    .unwrap();
    git(&tracker, &["add", "-A"]);
    git(&tracker, &["commit", "-q", "-m", "CAD-1365 fixture"]);
    tracker
}

/// A real current-schema store (v45) under `root/state`.
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
        redact_store_secrets: false,
    }
}

#[test]
fn cad1365_catalog_admits_known_media_tables_and_refuses_unknown_media_column() {
    let owned = tempfile::Builder::new()
        .prefix("cad1365-media-catalog-")
        .tempdir()
        .unwrap();
    let root = owned.path().join("closed-catalog");
    fs::create_dir_all(&root).unwrap();
    let tracker = make_tracker(&root);
    let state = make_state(&root);

    // Positive control: the real v45 store with the three known media
    // tables exports through the closed catalog.
    let good = root.join("package-good");
    export_portable(&state, &good, &opts(&tracker))
        .expect("known v45 media schema must pass the closed catalog");
    assert!(good.is_dir(), "positive control publishes the package");

    // Bad case: one unknown column on a known media table refuses.
    let live = db_file(&state);
    let version_before: i64 = Connection::open(&live)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version_before, crate::rollout::SCHEMA_VERSION);
    Connection::open(&live)
        .unwrap()
        .execute_batch("ALTER TABLE app_media_assets ADD COLUMN locator TEXT;")
        .unwrap();

    let bad = root.join("package-bad");
    let err = export_portable(&state, &bad, &opts(&tracker))
        .expect_err("an unknown media column must refuse the export");
    assert_eq!(err.to_string(), CATALOG_REFUSAL, "exact catalog refusal");
    assert!(!bad.exists(), "refused export writes no output directory");

    // No activation or live-store repair: the schema version is unchanged
    // and the refused column is still the one the fixture added.
    let conn = Connection::open(&live).unwrap();
    let version_after: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version_after, version_before);
    assert_eq!(version_after, crate::rollout::SCHEMA_VERSION);
    let locator: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('app_media_assets') WHERE name='locator'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(locator, 1);
}
