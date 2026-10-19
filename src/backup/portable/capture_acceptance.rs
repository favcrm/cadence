//! CAD-1549 independent acceptance for dirty-tracker snapshot guards.
//! Author is not the capture implementer. This file exercises the real
//! store, tracker admission, snapshot export, and read-only package inspect
//! entrypoints. The implementer must not edit or weaken it.
//!
//! Fixtures are disposable directories under a private temp root. No
//! production state, daemon, or installed binary is touched.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::capture::TrackerCapture;
use super::{export_portable_snapshot_selected, inspect_portable, PortableExportOptions, Scope};
use crate::rollout::db_file;
use crate::store::Store;

const KEEP: &[u8] = b"kept coordination note\n";
const CHANGED_OLD: &[u8] = b"tracked text before edit\n";
const CHANGED_NEW: &[u8] = b"tracked text after edit\n";
const GONE: &[u8] = b"this tracked file is deleted in the worktree\n";
const BINARY_OLD: &[u8] = b"\x00OLD\xffbinary\n";
const BINARY_NEW: &[u8] = &[0x00, 0xff, 0xfe, b'B', 0x00, 0x7f, b'N'];
const EXTRA: &[u8] = b"non-ignored untracked note\n";
const SCRIPT_OLD: &[u8] = b"#!/bin/sh\necho old\n";
const SCRIPT_NEW: &[u8] = b"#!/bin/sh\necho new\n";
const WAS_FILE_OLD: &[u8] = b"tracked file before it became a directory\n";
const WAS_FILE_CHILD: &[u8] = b"non-ignored child of the replacement directory\n";
const WAS_DIR_LEAF: &[u8] = b"tracked leaf inside a directory\n";
const WAS_DIR_FILE: &[u8] = b"non-ignored file that replaced the directory\n";

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let home = tempfile::Builder::new()
        .prefix("cad1549-git-home-")
        .tempdir_in("/tmp")
        .unwrap();
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("TMPDIR", home.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "cad1549-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad1549-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn git_stdout(dir: &Path, args: &[&str]) -> Vec<u8> {
    git(dir, args).stdout
}

fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

/// Ordinary tracker with a dirty worktree: modified text, modified binary,
/// deleted tracked file, mode-preserving executable edit, non-ignored
/// untracked file, and an ignored file that must not be captured.
fn dirty_tracker(root: &Path) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(tracker.join("docs")).unwrap();
    git(&tracker, &["init", "-q"]);
    write_file(&tracker.join("docs/keep.txt"), KEEP);
    write_file(&tracker.join("docs/changed.txt"), CHANGED_OLD);
    write_file(&tracker.join("docs/gone.txt"), GONE);
    write_file(&tracker.join("docs/data.bin"), BINARY_OLD);
    write_file(&tracker.join("docs/run.sh"), SCRIPT_OLD);
    write_file(&tracker.join("docs/was-file.txt"), WAS_FILE_OLD);
    write_file(&tracker.join("docs/was-dir/leaf.txt"), WAS_DIR_LEAF);
    write_file(&tracker.join(".gitignore"), b"ignored.log\n");
    git(
        &tracker,
        &[
            "add",
            "docs/run.sh",
            "docs/keep.txt",
            "docs/changed.txt",
            "docs/gone.txt",
            "docs/data.bin",
            "docs/was-file.txt",
            "docs/was-dir/leaf.txt",
            ".gitignore",
        ],
    );
    git(&tracker, &["update-index", "--chmod=+x", "docs/run.sh"]);
    git(&tracker, &["commit", "-q", "-m", "CAD-1549 fixture"]);
    write_file(&tracker.join("docs/changed.txt"), CHANGED_NEW);
    write_file(&tracker.join("docs/data.bin"), BINARY_NEW);
    write_file(&tracker.join("docs/run.sh"), SCRIPT_NEW);
    fs::set_permissions(
        tracker.join("docs/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::remove_file(tracker.join("docs/gone.txt")).unwrap();
    fs::remove_file(tracker.join("docs/was-file.txt")).unwrap();
    fs::create_dir(tracker.join("docs/was-file.txt")).unwrap();
    write_file(&tracker.join("docs/was-file.txt/child.txt"), WAS_FILE_CHILD);
    fs::remove_file(tracker.join("docs/was-dir/leaf.txt")).unwrap();
    fs::remove_dir(tracker.join("docs/was-dir")).unwrap();
    write_file(&tracker.join("docs/was-dir"), WAS_DIR_FILE);
    write_file(&tracker.join("docs/extra.txt"), EXTRA);
    write_file(&tracker.join("ignored.log"), b"local noise\n");
    let mode = fs::metadata(tracker.join("docs/run.sh")).unwrap().mode();
    assert_ne!(mode & 0o111, 0, "fixture keeps the executable bit");
    tracker
}

fn current_store(root: &Path) -> PathBuf {
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

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn source_fingerprint(tracker: &Path) -> (String, String, Vec<u8>, Vec<u8>) {
    let head = String::from_utf8(git_stdout(tracker, &["rev-parse", "HEAD"])).unwrap();
    let index = fs::read(tracker.join(".git/index")).unwrap();
    let config = fs::read(tracker.join(".git/config")).unwrap();
    let head_file = fs::read(tracker.join(".git/HEAD")).unwrap();
    (head, sha256(&index), config, head_file)
}

fn assert_source_unchanged(tracker: &Path, fp: &(String, String, Vec<u8>, Vec<u8>)) {
    let now = source_fingerprint(tracker);
    assert_eq!(now.0, fp.0, "source HEAD changed");
    assert_eq!(now.1, fp.1, "source index changed");
    assert_eq!(now.2, fp.2, "source git config changed");
    assert_eq!(now.3, fp.3, "source HEAD ref changed");
    assert_eq!(fs::read(tracker.join("docs/keep.txt")).unwrap(), KEEP);
    assert_eq!(
        fs::read(tracker.join("docs/changed.txt")).unwrap(),
        CHANGED_NEW
    );
    assert_eq!(fs::read(tracker.join("docs/data.bin")).unwrap(), BINARY_NEW);
    assert_eq!(fs::read(tracker.join("docs/run.sh")).unwrap(), SCRIPT_NEW);
    assert_eq!(fs::read(tracker.join("docs/extra.txt")).unwrap(), EXTRA);
    assert!(!tracker.join("docs/gone.txt").exists());
    assert_eq!(
        fs::read(tracker.join("ignored.log")).unwrap(),
        b"local noise\n"
    );
}

fn assert_unpublished(dir: &Path) {
    assert!(!dir.exists(), "refused export published {}", dir.display());
    if let Some(parent) = dir.parent() {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let leftover = fs::read_dir(parent).unwrap().any(|entry| {
            entry
                .ok()
                .map(|entry| {
                    let n = entry.file_name().to_string_lossy().into_owned();
                    n.contains(&name)
                })
                .unwrap_or(false)
        });
        assert!(!leftover, "refused export left {name} or a partial");
    }
}

fn packaged_file(pkg_tracker: &Path, rel: &str) -> (bool, Vec<u8>) {
    let file = pkg_tracker.join(rel);
    let meta = fs::metadata(&file).unwrap_or_else(|_| panic!("missing packaged file {rel}"));
    assert!(meta.is_file(), "{rel} is not a regular packaged file");
    (meta.mode() & 0o111 != 0, fs::read(file).unwrap())
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = from.symlink_metadata().unwrap();
        assert!(!meta.file_type().is_symlink(), "refusing to copy a symlink");
        if meta.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
            fs::set_permissions(&to, fs::Permissions::from_mode(meta.mode())).unwrap();
        }
    }
}

fn tree_digest(root: &Path) -> String {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = path.symlink_metadata().unwrap();
            if meta.file_type().is_symlink() {
                files.push(format!("link {}", path.display()));
            } else if meta.is_dir() {
                stack.push(path);
            } else {
                files.push(format!(
                    "{} {} {}",
                    path.strip_prefix(root).unwrap().display(),
                    meta.mode(),
                    sha256(&fs::read(&path).unwrap())
                ));
            }
        }
    }
    files.sort();
    sha256(files.join("\n").as_bytes())
}

#[test]
fn cad1549_dirty_tracker_snapshot_export_and_inspect_guards() {
    let owned = tempfile::Builder::new()
        .prefix("cad1549-capture-acceptance-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = owned.path();
    let tracker = dirty_tracker(root);
    let state = current_store(root);
    let before = source_fingerprint(&tracker);

    let app_out = root.join("app-refused");
    export_portable_snapshot_selected(
        &state,
        &app_out,
        &opts(&tracker),
        &["unsupported-app".to_string()],
    )
    .expect_err("unsupported app refuses");
    assert_unpublished(&app_out);
    assert_source_unchanged(&tracker, &before);

    let existing = root.join("existing-out");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("sentinel"), b"leave me\n").unwrap();
    assert!(
        export_portable_snapshot_selected(&state, &existing, &opts(&tracker), &[]).is_err(),
        "existing output refuses"
    );
    assert_eq!(fs::read(existing.join("sentinel")).unwrap(), b"leave me\n");
    assert_source_unchanged(&tracker, &before);

    // Output that overlaps the source tracker is refused before staging.
    let inside = tracker.join("docs/overlap-out");
    assert!(
        export_portable_snapshot_selected(&state, &inside, &opts(&tracker), &[]).is_err(),
        "output inside the source tracker refuses"
    );
    assert!(!inside.exists(), "overlapping output was not created");
    assert_source_unchanged(&tracker, &before);

    // The same overlap through a symlinked parent is refused the same way.
    let alias = root.join("tracker-alias");
    std::os::unix::fs::symlink(&tracker, &alias).unwrap();
    let via_link = alias.join("docs/overlap-via-link");
    assert!(
        export_portable_snapshot_selected(&state, &via_link, &opts(&tracker), &[]).is_err(),
        "output behind a symlinked parent that lands in the tracker refuses"
    );
    assert!(
        !tracker.join("docs/overlap-via-link").exists(),
        "symlinked overlapping output was not created"
    );
    assert_source_unchanged(&tracker, &before);

    let package = root.join("package");
    export_portable_snapshot_selected(&state, &package, &opts(&tracker), &[])
        .expect("dirty snapshot exports");
    assert_source_unchanged(&tracker, &before);

    let snap = package.join("tracker");
    assert!(
        !snap.join(".git").exists(),
        "packaged tracker has no git metadata"
    );
    let (keep_exec, keep) = packaged_file(&snap, "docs/keep.txt");
    assert_eq!(keep, KEEP);
    assert!(!keep_exec, "clean file stays non-executable");
    let (changed_exec, changed) = packaged_file(&snap, "docs/changed.txt");
    assert_eq!(changed, CHANGED_NEW);
    assert!(!changed_exec, "modified text stays non-executable");
    let (bin_exec, bin) = packaged_file(&snap, "docs/data.bin");
    assert_eq!(bin, BINARY_NEW);
    assert!(!bin_exec, "binary stays non-executable");
    let (script_exec, script) = packaged_file(&snap, "docs/run.sh");
    assert_eq!(script, SCRIPT_NEW);
    assert!(script_exec, "executable bit survives");
    let (extra_exec, extra) = packaged_file(&snap, "docs/extra.txt");
    assert_eq!(extra, EXTRA);
    assert!(!extra_exec, "untracked file stays non-executable");
    assert!(!snap.join("docs/gone.txt").exists(), "deletion is absent");
    assert!(!snap.join("ignored.log").exists(), "ignored file is absent");
    assert!(
        !snap.join("docs/was-file.txt").is_file(),
        "tracked file replaced by a directory is not captured as a file"
    );
    let (child_exec, child) = packaged_file(&snap, "docs/was-file.txt/child.txt");
    assert_eq!(child, WAS_FILE_CHILD);
    assert!(
        !child_exec,
        "replacement directory child stays non-executable"
    );
    assert!(
        !snap.join("docs/was-dir/leaf.txt").exists(),
        "tracked directory child is absent after directory-to-file replacement"
    );
    let (replaced_exec, replaced) = packaged_file(&snap, "docs/was-dir");
    assert_eq!(replaced, WAS_DIR_FILE);
    assert!(!replaced_exec, "replacement file stays non-executable");
    assert_eq!(
        fs::read(tracker.join("docs/was-file.txt/child.txt")).unwrap(),
        WAS_FILE_CHILD
    );
    assert_eq!(
        fs::read(tracker.join("docs/was-dir")).unwrap(),
        WAS_DIR_FILE
    );
    assert!(!tracker.join("docs/was-dir/leaf.txt").exists());

    let packed = tree_digest(&package);
    inspect_portable(&package).expect("clean package inspects");
    assert_eq!(tree_digest(&package), packed, "inspect is read-only");

    let wal_pkg = root.join("package-wal");
    copy_tree(&package, &wal_pkg);
    let wal_db = wal_pkg.join("cadence.sqlite3");
    {
        let conn = rusqlite::Connection::open(&wal_db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .unwrap();
    }
    for sidecar in ["cadence.sqlite3-wal", "cadence.sqlite3-shm"] {
        let path = wal_pkg.join(sidecar);
        if path.exists() {
            fs::remove_file(&path).unwrap();
        }
    }
    assert!(!wal_pkg.join("cadence.sqlite3-wal").exists());
    assert!(!wal_pkg.join("cadence.sqlite3-shm").exists());
    let db_bytes = fs::read(&wal_db).unwrap();
    assert_eq!(&db_bytes[18..20], &[2, 2], "native db header is WAL");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(wal_pkg.join("manifest.json")).unwrap()).unwrap();
    let members = manifest
        .get_mut("members")
        .and_then(|value| value.as_array_mut())
        .unwrap();
    let db_member = members
        .iter_mut()
        .find(|member| member.get("path").and_then(|path| path.as_str()) == Some("cadence.sqlite3"))
        .expect("manifest lists the native database");
    db_member["bytes"] = serde_json::json!(db_bytes.len() as u64);
    db_member["sha256"] = serde_json::json!(sha256(&db_bytes));
    fs::write(
        wal_pkg.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let wal_digest = tree_digest(&wal_pkg);
    assert!(
        inspect_portable(&wal_pkg).is_err(),
        "WAL database refuses inspect before SQLite open"
    );
    assert_eq!(
        tree_digest(&wal_pkg),
        wal_digest,
        "WAL refusal is read-only"
    );
    assert!(
        !wal_pkg.join("cadence.sqlite3-wal").exists(),
        "inspect created no -wal"
    );
    assert!(
        !wal_pkg.join("cadence.sqlite3-shm").exists(),
        "inspect created no -shm"
    );

    let extra_pkg = root.join("package-extra");
    copy_tree(&package, &extra_pkg);
    write_file(&extra_pkg.join("tracker/not-listed.txt"), b"extra member\n");
    let extra_digest = tree_digest(&extra_pkg);
    assert!(
        inspect_portable(&extra_pkg).is_err(),
        "extra package member fails inspect"
    );
    assert_eq!(
        tree_digest(&extra_pkg),
        extra_digest,
        "failed inspect is read-only"
    );
    assert_eq!(tree_digest(&package), packed, "original package untouched");

    let tampered = root.join("package-tampered");
    copy_tree(&package, &tampered);
    let member = tampered.join("manifest.json");
    let mut manifest = fs::read(&member).unwrap();
    let last = manifest.last_mut().unwrap();
    *last ^= 0xff;
    fs::write(&member, &manifest).unwrap();
    let tampered_digest = tree_digest(&tampered);
    assert!(
        inspect_portable(&tampered).is_err(),
        "tampered manifest fails inspect"
    );
    assert_eq!(
        tree_digest(&tampered),
        tampered_digest,
        "failed inspect is read-only"
    );

    let link_root = root.join("symlink-case");
    let link_tracker = dirty_tracker(&link_root);
    std::os::unix::fs::symlink("docs/keep.txt", link_tracker.join("docs/malicious")).unwrap();
    let link_out = link_root.join("package");
    assert!(
        TrackerCapture::new(&link_tracker).is_err(),
        "symlink capture refuses"
    );
    assert!(
        export_portable_snapshot_selected(&state, &link_out, &opts(&link_tracker), &[]).is_err(),
        "symlink export refuses"
    );
    assert_unpublished(&link_out);

    let filter_root = root.join("filter-case");
    let filter_tracker = dirty_tracker(&filter_root);
    let mut config = fs::read(filter_tracker.join(".git/config")).unwrap();
    config.extend_from_slice(b"\n[filter \"evil\"]\n\tclean = /bin/false\n\tsmudge = /bin/false\n");
    fs::write(filter_tracker.join(".git/config"), &config).unwrap();
    let filter_out = filter_root.join("package");
    assert!(
        TrackerCapture::new(&filter_tracker).is_err(),
        "filter configuration capture refuses"
    );
    assert!(
        export_portable_snapshot_selected(&state, &filter_out, &opts(&filter_tracker), &[])
            .is_err(),
        "filter configuration export refuses"
    );
    assert_unpublished(&filter_out);
    assert!(
        fs::read(filter_tracker.join(".git/config"))
            .unwrap()
            .windows(b"filter \"evil\"".len())
            .any(|w| w == b"filter \"evil\""),
        "source config left as planted"
    );

    let move_root = root.join("movement-case");
    let move_tracker = dirty_tracker(&move_root);
    let scratch = {
        let capture = TrackerCapture::new(&move_tracker).expect("ordinary dirty tracker captures");
        let scratch = capture.path().to_path_buf();
        assert!(scratch.is_dir(), "capture owns a private snapshot");
        write_file(
            &move_tracker.join("docs/changed.txt"),
            b"mutated after capture\n",
        );
        assert!(
            capture.verify_unchanged().is_err(),
            "post-capture source mutation refuses"
        );
        scratch
    };
    assert!(
        !scratch.exists(),
        "dropping the capture removes its scratch"
    );
}
