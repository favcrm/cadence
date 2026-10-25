//! Independently authored OMP Spec/security compatibility oracles for CAD-1128.
//! Mounted only by the CAD-1425 native integration target, not by lib tests.
//! Exercise actual public backup/export entry points on an ordinary legacy
//! state with no encrypted configuration, vault or key. Run on macOS to
//! expose the pinned-head Linux-only ArtifactDir regression; Linux is only
//! a same-oracle working control, not evidence of macOS acceptance.
//! Original r2 preserves existing backup preflight/guards and existing legacy
//! behavior; refusal of unsupported encrypted primitives is not authority to
//! disable all ordinary backup/export or mandatory pre-update backups.
//! No frozen driver, merged assertion or production guard is modified.

use cadence_agent::store::Store;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn must<T, E>(result: std::result::Result<T, E>, stage: &str) -> T {
    result.unwrap_or_else(|_| panic!("{stage}"))
}

fn state(root: &Path) -> PathBuf {
    // Use the real canonical fixture root, avoiding incidental macOS /var
    // aliases. This does not relax any production namespace check.
    let root = must(root.canonicalize(), "fixture canonical root unavailable");
    let state = root.join("state");
    must(
        std::fs::DirBuilder::new().mode(0o700).create(&state),
        "private legacy state creation failed",
    );
    drop(must(
        Store::open(&state.join("cadence.sqlite3")),
        "ordinary current-schema core fixture setup failed",
    ));
    assert!(
        !state.join("custody-encrypted.json").exists(),
        "legacy control must not configure encrypted custody"
    );
    assert!(
        !state.join("vault").exists(),
        "legacy control must not create encrypted storage"
    );
    state
}

fn root() -> tempfile::TempDir {
    let root = must(
        tempfile::Builder::new()
            .prefix("omp-c1128-artifact-")
            .tempdir_in(std::env::temp_dir()),
        "private compatibility fixture root creation failed",
    );
    must(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        "private compatibility fixture root permissions failed",
    );
    root
}

#[test]
fn ordinary_legacy_backup_remains_available_on_supported_unix() {
    let root = root();
    let state = state(root.path());
    let out = state
        .parent()
        .expect("fixture state parent")
        .join("backup-output");
    let result = must(
        cadence_agent::backup::backup(&state, &out, 2, "manual"),
        "ordinary legacy backup must remain available on supported Unix",
    );
    assert!(
        result["backup"] == true,
        "real backup did not report completion"
    );
    let manifest = PathBuf::from(
        result["manifest"]
            .as_str()
            .expect("completed backup omitted manifest path"),
    );
    let db = PathBuf::from(
        result["db"]
            .as_str()
            .expect("completed backup omitted database path"),
    );
    assert!(
        manifest.is_file() && db.is_file(),
        "real backup did not produce its promised files"
    );
    must(
        cadence_agent::backup::verify(&manifest),
        "produced legacy backup failed actual verification",
    );
    assert!(
        !state.join("custody-encrypted.json").exists() && !state.join("vault").exists(),
        "ordinary backup must not initialize or require encrypted custody"
    );
}

#[test]
fn ordinary_legacy_export_remains_available_on_supported_unix() {
    let root = root();
    let state = state(root.path());
    let out = state
        .parent()
        .expect("fixture state parent")
        .join("export-output");
    let result = must(
        cadence_agent::backup::export(&state, &out),
        "ordinary legacy export must remain available on supported Unix",
    );
    assert!(
        result["export"] == true,
        "real export did not report completion"
    );
    let manifest = PathBuf::from(
        result["manifest"]
            .as_str()
            .expect("completed export omitted manifest path"),
    );
    assert!(
        manifest.is_file(),
        "real export did not produce its promised manifest"
    );
    must(
        cadence_agent::backup::verify(&manifest),
        "produced legacy export failed actual verification",
    );
    assert!(
        !state.join("custody-encrypted.json").exists() && !state.join("vault").exists(),
        "ordinary export must not initialize or require encrypted custody"
    );
}
