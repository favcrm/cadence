//! CAD-1128 independent acceptance leaf: portable coordination export must
//! refuse artifact outputs that overlap the configured path-only custody
//! source, without touching the key or publishing a secret-bearing package.
//!
//! Author: separately assigned independent acceptance author. The product
//! implementation (the custody admission added to
//! `export_portable`/`export_portable_snapshot_selected`) must not weaken or
//! edit these checks. The owner mounts this leaf as
//! `#[cfg(test)] mod cad1128_portable_custody_acceptance;`.
//!
//! WHAT IS ACTUALLY EXERCISED (real product entrypoints only):
//! - a real `cadence.state` configured for path-only encrypted custody —
//!   `custody-encrypted.json` (0600) naming an external reserved key file
//!   outside the state dir, plus the admitted `<state>/vault` directory,
//!   exactly the shape `platform::encrypted_config::load` admits;
//! - a real eligible coordination export through a clean Git tracker that
//!   is DISJOINT from the key/vault/config paths: it still publishes
//!   (`manifest.json`, DB snapshot, byte-identical tracker members);
//! - `export_portable` refuses an output under the reserved `<state>/vault`;
//! - `export_portable_snapshot_selected` refuses a selected Git tracker
//!   that contains the named key file (the snapshot capture mirrors the
//!   whole worktree, so this admission must run before capture);
//! - the key bytes/inode and the selected tracker are unchanged, no
//!   package or `.<name>.partial-*` staging entry is left behind, and the
//!   published package carries none of the synthetic key material.
//!
//! LIMITS (stated, not implied): this is PATH-ONLY artifact admission proof.
//! It does not prove encryption, entropy, host isolation, UID separation,
//! or that an unconfigured/legacy state cannot be exported; it asserts no
//! vault unlock, no crypto and no real provider/credential. The key file is
//! synthetic marker bytes written by the fixture, not a real custody key.
//! The whole fixture lives under ONE retained `tempfile::TempDir`
//! (`std::env::temp_dir()`, short unique prefix, 0700) and only that RAII
//! root is removed — never a shared/fixed `/tmp` tree. No production state,
//! ports or credentials are used, and no test is executed by this author.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::backup::portable::{
    export_portable, export_portable_snapshot_selected, PortableExportOptions, Scope,
};
use crate::rollout::db_file;
use crate::store::Store;

const TICKET_REL: &str = "tickets/CAD-1128.md";
const TICKET_BODY: &str = "# CAD-1128 synthetic ticket fixture\n\nPortable custody boundary.\n";
/// Synthetic only — proves the reserved key bytes never reach a package.
const KEY_MARKER: &str = "SYNTH-CAD1128-CUSTODY-KEY-MATERIAL-NEVER-EXPORTED";

/// 0700 owner-only directory — the exact shape key parents and the vault
/// namespace require.
fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
            // Detached post-commit maintenance can race fixture reads.
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "cad1128-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad1128-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A clean tracker with one committed ticket file. `ignore_custody` commits
/// a `.gitignore` for `custody/` so a key placed there is untracked-but-
/// ignored: the tracker still admits cleanly, and the custody refusal (not
/// dirtiness) is what the bad case must observe.
fn make_tracker(dir: &Path, ignore_custody: bool) -> PathBuf {
    fs::create_dir_all(dir.join("tickets")).unwrap();
    git(dir, &["init", "-q"]);
    fs::write(dir.join(TICKET_REL), TICKET_BODY).unwrap();
    if ignore_custody {
        fs::write(dir.join(".gitignore"), "custody/\n").unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "CAD-1128 fixture"]);
    dir.to_path_buf()
}

/// A current-schema coordination store in a 0700 state directory.
fn make_state(dir: &Path) -> PathBuf {
    private_dir(dir);
    drop(Store::open(&db_file(dir)).expect("fixture store opens"));
    dir.to_path_buf()
}

/// The reserved external key: 0600, private parent, synthetic marker body.
fn write_key(dir: &Path) -> PathBuf {
    private_dir(dir);
    let key = dir.join("connections.key");
    fs::write(&key, format!("{KEY_MARKER}\n")).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    key
}

/// A REAL path-only configured custody state: the fixed record names the
/// external key and the default `<state>/vault` namespace exists and is
/// admitted. No vault database or key material is opened here.
fn configured_state(state_dir: &Path, key_dir: &Path) -> (PathBuf, PathBuf) {
    let state = make_state(state_dir);
    private_dir(&state.join("vault"));
    let key = write_key(key_dir);
    let record = serde_json::json!({"key_file": key.display().to_string()}).to_string();
    let record_path = state.join("custody-encrypted.json");
    fs::write(&record_path, record).unwrap();
    fs::set_permissions(&record_path, fs::Permissions::from_mode(0o600)).unwrap();
    (state, key)
}

fn opts(tracker: &Path) -> PortableExportOptions {
    PortableExportOptions {
        scope: Scope::Coordination,
        tracker: tracker.to_path_buf(),
        redact_history_secrets: false,
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Every file byte under `dir` (the package is small and fully synthetic).
/// Any inventory or read failure PANICS — never silently proves absence by
/// skipping an entry and leaving its bytes unscanned.
fn package_bytes(dir: &Path) -> Vec<u8> {
    fn walk(dir: &Path, out: &mut Vec<u8>) {
        let entries = fs::read_dir(dir).unwrap_or_else(|error| {
            panic!("package inventory of {} failed: {error}", dir.display())
        });
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|error| panic!("package inventory entry unreadable: {error}"));
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap_or_else(|error| {
                panic!("package member {} uninspectable: {error}", path.display())
            });
            if meta.is_dir() {
                walk(&path, out);
            } else {
                out.extend(fs::read(&path).unwrap_or_else(|error| {
                    panic!("package member {} unreadable: {error}", path.display())
                }));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out
}

/// No `.<name>.partial-*` staging entry next to the intended output. A
/// genuinely absent parent means no staging was created; every other
/// inventory error PANICS rather than reporting absence.
fn has_partial(out: &Path) -> bool {
    let Some(parent) = out.parent() else {
        return false;
    };
    let name = out
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!(".{name}.partial-");
    match fs::read_dir(parent) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry
                    .unwrap_or_else(|error| panic!("staging inventory entry unreadable: {error}"));
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    return true;
                }
            }
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("staging inventory of {} failed: {error}", parent.display()),
    }
}

fn key_bytes(key: &Path) -> (Vec<u8>, u64) {
    let meta = fs::symlink_metadata(key).expect("reserved key must exist");
    (fs::read(key).expect("reserved key readable"), meta.ino())
}

#[test]
fn cad1128_portable_export_refuses_custody_overlap_paths() {
    // ONE retained RAII root under the session temp dir (0700, unique
    // prefix). Nothing shared is removed; only this directory is cleaned.
    let fixture = tempfile::Builder::new()
        .prefix("cad1128-portable-")
        .tempdir_in(std::env::temp_dir())
        .expect("portable custody fixture root");
    let fixture_root = fs::canonicalize(fixture.path()).expect("fixture root is resolvable");
    let base = fixture_root.as_path();

    // ---------- eligible safe positive through a configured state ----------
    // Real tracker, real Store, real configured custody: only the paths are
    // disjoint, so the new admission must NOT refuse.
    let safe_tracker = make_tracker(&base.join("tracker-safe"), false);
    let (safe_state, safe_key) =
        configured_state(&base.join("state-safe"), &base.join("custody-safe"));
    let (safe_key_before, safe_key_ino) = key_bytes(&safe_key);
    let safe_head = git(&safe_tracker, &["rev-parse", "HEAD"]);

    let out = base.join("exports").join("safe-package");
    let report = export_portable(&safe_state, &out, &opts(&safe_tracker))
        .expect("an eligible output disjoint from custody must still export");
    assert_eq!(report["scope"], "coordination");
    assert_eq!(report["full_workspace"], false);
    assert!(
        out.is_dir() && !has_partial(&out),
        "positive package must publish exactly once"
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["format"], "cadence.portable/1");
    assert_eq!(manifest["tracker_revision"], safe_head);
    assert_eq!(
        fs::read(out.join("tracker").join(TICKET_REL)).unwrap(),
        TICKET_BODY.as_bytes(),
        "real tracker member bytes survive the export"
    );
    assert!(
        !contains(&package_bytes(&out), KEY_MARKER),
        "published package must not carry custody key material"
    );
    assert_eq!(
        key_bytes(&safe_key),
        (safe_key_before.clone(), safe_key_ino),
        "positive export changed the reserved key"
    );

    // ---------- reserved vault output is refused ----------
    let vault = safe_state.join("vault");
    let vault_out = vault.join("package");
    let err = export_portable(&safe_state, &vault_out, &opts(&safe_tracker))
        .expect_err("an output inside the reserved vault must refuse");
    assert_eq!(
        err.code(),
        Some("invalid_store"),
        "vault output must refuse the path-only custody admission: {err}"
    );
    assert!(!vault_out.exists(), "refused vault export wrote a package");
    assert!(
        !has_partial(&vault_out),
        "refused vault export left staging behind"
    );
    assert_eq!(
        fs::read_dir(&vault).unwrap().count(),
        0,
        "reserved vault namespace must stay empty"
    );
    assert_eq!(
        key_bytes(&safe_key),
        (safe_key_before, safe_key_ino),
        "refused vault export changed the reserved key"
    );

    // ---------- selected tracker containing the named key is refused ----------
    // The key is untracked-but-ignored, so the tracker is clean and only the
    // custody overlap can justify the refusal. Capture mirrors the whole
    // worktree, so this admission must run before capture.
    let key_tracker = make_tracker(&base.join("tracker-with-key"), true);
    let (key_state, contained_key) =
        configured_state(&base.join("state-tracker"), &key_tracker.join("custody"));
    let (contained_key_before, contained_key_ino) = key_bytes(&contained_key);
    let key_head = git(&key_tracker, &["rev-parse", "HEAD"]);
    assert_eq!(
        git(
            &key_tracker,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "ignored custody key must not make the selected tracker dirty"
    );

    let snap_out = base.join("exports").join("snapshot-package");
    let err = export_portable_snapshot_selected(&key_state, &snap_out, &opts(&key_tracker), &[])
        .expect_err("a selected tracker containing the custody key must refuse");
    assert_eq!(
        err.code(),
        Some("invalid_store"),
        "selected tracker overlap must refuse the path-only custody admission: {err}"
    );
    assert!(
        !snap_out.exists(),
        "refused snapshot export wrote a package"
    );
    assert!(
        !has_partial(&snap_out),
        "refused snapshot export left staging behind"
    );
    assert_eq!(
        key_bytes(&contained_key),
        (contained_key_before, contained_key_ino),
        "refused snapshot export changed the reserved key"
    );
    assert_eq!(
        git(
            &key_tracker,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "refused snapshot export left the selected tracker dirty"
    );
    assert_eq!(
        git(&key_tracker, &["rev-parse", "HEAD"]),
        key_head,
        "refused snapshot export moved the selected tracker HEAD"
    );
}
