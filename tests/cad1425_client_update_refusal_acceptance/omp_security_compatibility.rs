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

/// A configured encrypted state is intentionally unsupported for macOS
/// artifacts. This is a real filesystem/public-API bad case, independent of
/// the ordinary legacy-positive oracles above. No vault initialization or key
/// read is necessary for the path-only configuration admission being tested.
#[cfg(target_os = "macos")]
#[test]
fn configured_encrypted_macos_artifacts_refuse_before_output_effects() {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let root = root();
    let state = state(root.path());
    let owned = state.parent().expect("private fixture parent");
    let planted = owned.join("planted-artifacts");
    // A genuinely valid existing artifact is the retention/cleanup sentinel.
    // This must succeed BEFORE encryption is configured, not be a canned file.
    let existing = must(
        cadence_agent::backup::backup(&state, &planted, 2, "manual"),
        "macOS legacy artifact sentinel setup failed",
    );
    let existing_manifest = existing["manifest"]
        .as_str()
        .expect("existing artifact sentinel omitted its manifest");
    must(
        cadence_agent::backup::verify(Path::new(existing_manifest)),
        "actual existing artifact sentinel failed verification",
    );
    let sentinel = planted.join("foreign-output.bin");
    must(
        std::fs::write(&sentinel, b"OMP-MAC-FOREIGN-OUTPUT-PRESERVE"),
        "planted output sentinel setup failed",
    );

    let key_dir = owned.join("owner-key");
    must(
        std::fs::DirBuilder::new().mode(0o700).create(&key_dir),
        "private synthetic key directory setup failed",
    );
    let key_path = key_dir.join("fixture-key");
    let mut synthetic_key = [0u8; 32];
    must(
        getrandom::fill(&mut synthetic_key),
        "synthetic fixture key entropy failed",
    );
    let mut key = must(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key_path),
        "private synthetic key reservation failed",
    );
    must(
        key.write_all(&synthetic_key),
        "synthetic key provisioning failed",
    );
    must(key.sync_all(), "synthetic key durability setup failed");
    drop(key);
    let config_path = state.join("custody-encrypted.json");
    let config_bytes = must(
        serde_json::to_vec(&serde_json::json!({
            "key_file": key_path,
            "vault_dir": state.join("vault"),
        })),
        "path-only configuration serialization failed",
    );
    let mut config = must(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&config_path),
        "private configuration reservation failed",
    );
    must(
        config.write_all(&config_bytes),
        "path-only configuration setup failed",
    );
    must(
        config.sync_all(),
        "path-only configuration durability setup failed",
    );
    drop(config);
    for path in [&key_path, &config_path] {
        let meta = must(
            std::fs::symlink_metadata(path),
            "private fixture file metadata failed",
        );
        assert!(
            meta.is_file()
                && meta.mode() & 0o7777 == 0o600
                && meta.nlink() == 1
                && meta.uid() == unsafe { libc::geteuid() },
            "configured fixture must have real owner-only single-link regular files"
        );
    }
    assert!(
        must(
            std::fs::metadata(&key_path),
            "synthetic key metadata failed"
        )
        .len()
            == 32,
        "configured fixture key must have exactly 32 bytes"
    );

    let fresh = owned.join("absent-parent").join("fresh-artifact");
    // Resolving this path would fail at a non-directory ancestor. The platform
    // refusal must win before output-path inspection, not merely before writes.
    let invalid_parent = sentinel.join("uninspectable-child");
    let before = super::snapshot(owned);
    for output in [&fresh, &planted, &invalid_parent] {
        let operations = [
            cadence_agent::backup::backup(&state, output, 1, "manual"),
            cadence_agent::backup::export(&state, output),
        ];
        for outcome in operations {
            let error = match outcome {
                Err(error) => error,
                Ok(_) => panic!("configured macOS encrypted artifact generation was admitted"),
            };
            // Distinguish the actual new platform guard from malformed config,
            // an existing-output refusal, or an output pathname/setup failure.
            assert!(
                error.kind() == "rejected"
                    && error
                        .to_string()
                        .contains("encrypted custody artifacts are unsupported on this platform"),
                "configured operation did not refuse at the early platform boundary"
            );
            assert!(super::snapshot(owned) == before,
                "platform refusal allocated/inspected destructively/pruned/cleaned source or output bytes");
        }
    }
    assert!(
        !owned.join("absent-parent").exists(),
        "platform refusal allocated an output ancestor"
    );
    assert!(
        !state.join("vault").exists(),
        "artifact refusal initialized encrypted storage"
    );
}
