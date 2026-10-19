//! CAD-1557 independent acceptance for snapshot export of a hook-bearing tracker.
//! Author is not the capture implementer. This file exercises the real store,
//! tracker, snapshot export, ordinary export, and read-only package inspect
//! entrypoints. The implementer must not edit or weaken it.
//!
//! A hook that runs writes `CAD1557_HOOK_RAN` to a marker outside the hooks
//! directory and exits 1. That fails this check in either shape: the export
//! error when git honors the status, and the marker assertion when git does
//! not. Ordinary export must still refuse on the hook guard itself (active
//! hooks, or `core.hooksPath`) before the clean-tree check and before any
//! package directory is published. A symlink hook stays refused. Source HEAD,
//! index, config, hooks, and worktree bytes are compared on the filesystem.
//!
//! Fixtures are disposable directories under a private temp root. No
//! production state, daemon, or installed binary is touched. The CAD-1549
//! capture acceptance stays the owner of worktree-symlink and filter rows.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::{
    export_portable, export_portable_snapshot_selected, inspect_portable, PortableExportOptions,
    Scope,
};
use crate::rollout::db_file;
use crate::store::Store;

const SENTINEL: &[u8] = b"CAD1557_HOOK_RAN";
const GITIGNORE: &[u8] = b"noise.log\n";
const EDITED_OLD: &[u8] = b"#!/bin/sh\necho committed\n";
const EDITED_NEW: &[u8] = b"#!/bin/sh\necho edited\n";
const GONE: &[u8] = b"tracked file removed from the worktree\n";
const EXTRA: &[u8] = b"non-ignored untracked bytes\n";
const NOISE: &[u8] = b"ignored local noise\n";
const HOOK_NAMES: &[&str] = &[
    "fsmonitor-watchman",
    "post-checkout",
    "post-commit",
    "post-index-change",
    "post-merge",
    "post-rewrite",
    "pre-auto-gc",
    "pre-commit",
    "reference-transaction",
];

struct Finger {
    head_file: Vec<u8>,
    ref_file: Vec<u8>,
    index_sha256: String,
    config: Vec<u8>,
    hooks: Vec<String>,
    edited: Vec<u8>,
    edited_mode: u32,
    extra: Vec<u8>,
    extra_mode: u32,
    gitignore: Vec<u8>,
    noise: Vec<u8>,
}

fn git(dir: &Path, args: &[&str]) {
    let home = tempfile::Builder::new()
        .prefix("cad1557-git-home-")
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
        .env("GIT_AUTHOR_NAME", "cad1557-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad1557-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Executable hooks that record the sentinel and fail. Names cover checkout,
/// commit, index and ref updates, gc, and fsmonitor. `ls-files` does not call
/// them; a source command that does, without the no-hooks override, trips here.
fn hook_script(marker: &Path) -> Vec<u8> {
    let marker = marker.to_str().expect("marker path is utf-8");
    assert!(!marker.contains('\''), "marker path is shell-safe");
    let mut body = b"#!/bin/sh\nprintf '%s\\n' 'CAD1557_HOOK_RAN' >> '".to_vec();
    body.extend_from_slice(marker.as_bytes());
    body.extend_from_slice(b"'\nexit 1\n");
    body
}

fn install_failing_hooks(dir: &Path, script: &[u8]) {
    fs::create_dir_all(dir).unwrap();
    for name in HOOK_NAMES {
        write_file(&dir.join(name), script, 0o755);
    }
}

fn remove_active_hook_entries(tracker: &Path) {
    let hooks = tracker.join(".git/hooks");
    if !hooks.is_dir() {
        return;
    }
    for entry in fs::read_dir(&hooks).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".sample") {
            continue;
        }
        let path = entry.path();
        let meta = path.symlink_metadata().unwrap();
        if meta.is_dir() && !meta.file_type().is_symlink() {
            fs::remove_dir_all(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
    }
}

fn dirty_tracker(root: &Path) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(&tracker).unwrap();
    git(&tracker, &["init", "-q"]);
    write_file(&tracker.join(".gitignore"), GITIGNORE, 0o644);
    write_file(&tracker.join("edited.sh"), EDITED_OLD, 0o644);
    write_file(&tracker.join("gone.txt"), GONE, 0o644);
    git(&tracker, &["add", ".gitignore", "edited.sh", "gone.txt"]);
    git(&tracker, &["update-index", "--chmod=+x", "edited.sh"]);
    git(&tracker, &["commit", "-q", "-m", "CAD-1557 fixture"]);
    write_file(&tracker.join("edited.sh"), EDITED_NEW, 0o755);
    fs::remove_file(tracker.join("gone.txt")).unwrap();
    write_file(&tracker.join("extra.txt"), EXTRA, 0o644);
    write_file(&tracker.join("noise.log"), NOISE, 0o644);
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

fn hook_listing(tracker: &Path) -> Vec<String> {
    let hooks = tracker.join(".git/hooks");
    let mut rows = Vec::new();
    if !hooks.is_dir() {
        return rows;
    }
    let mut entries = fs::read_dir(&hooks)
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = path.symlink_metadata().unwrap();
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap();
            rows.push(format!("link {name} -> {}", target.display()));
        } else if meta.is_file() {
            rows.push(format!(
                "file {name} {:o} {}",
                meta.mode() & 0o777,
                sha256(&fs::read(&path).unwrap())
            ));
        } else {
            rows.push(format!("other {name}"));
        }
    }
    rows
}

fn fingerprint(tracker: &Path) -> Finger {
    let head_file = fs::read(tracker.join(".git/HEAD")).unwrap();
    let head = String::from_utf8(head_file.clone()).unwrap();
    let rest = head.trim().strip_prefix("ref: ").expect("symbolic HEAD");
    Finger {
        ref_file: fs::read(tracker.join(".git").join(rest)).unwrap(),
        head_file,
        index_sha256: sha256(&fs::read(tracker.join(".git/index")).unwrap()),
        config: fs::read(tracker.join(".git/config")).unwrap(),
        hooks: hook_listing(tracker),
        edited: fs::read(tracker.join("edited.sh")).unwrap(),
        edited_mode: fs::metadata(tracker.join("edited.sh")).unwrap().mode() & 0o777,
        extra: fs::read(tracker.join("extra.txt")).unwrap(),
        extra_mode: fs::metadata(tracker.join("extra.txt")).unwrap().mode() & 0o777,
        gitignore: fs::read(tracker.join(".gitignore")).unwrap(),
        noise: fs::read(tracker.join("noise.log")).unwrap(),
    }
}

fn assert_source_unchanged(tracker: &Path, before: &Finger) {
    let now = fingerprint(tracker);
    assert_eq!(now.head_file, before.head_file, "source HEAD changed");
    assert_eq!(now.ref_file, before.ref_file, "source branch ref changed");
    assert_eq!(
        now.index_sha256, before.index_sha256,
        "source index changed"
    );
    assert_eq!(now.config, before.config, "source git config changed");
    assert_eq!(now.hooks, before.hooks, "source hooks changed");
    assert_eq!(now.edited, EDITED_NEW);
    assert_eq!(now.edited, before.edited);
    assert_eq!(
        now.edited_mode, 0o755,
        "source executable edit mode changed"
    );
    assert_eq!(now.edited_mode, before.edited_mode);
    assert_eq!(now.extra, EXTRA);
    assert_eq!(now.extra, before.extra);
    assert_eq!(now.extra_mode, before.extra_mode);
    assert_eq!(now.gitignore, GITIGNORE);
    assert_eq!(now.noise, NOISE);
    assert!(
        !tracker.join("gone.txt").exists(),
        "deleted source file returned"
    );
}

fn assert_hooks_did_not_run(marker: &Path) {
    assert!(
        !marker.exists(),
        "hook executed and wrote {}",
        marker.display()
    );
}

fn assert_unpublished(dir: &Path) {
    assert!(!dir.exists(), "refused export published {}", dir.display());
    if let Some(parent) = dir.parent() {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let leftover = fs::read_dir(parent).unwrap().any(|entry| {
            entry
                .ok()
                .map(|entry| entry.file_name().to_string_lossy().contains(&name))
                .unwrap_or(false)
        });
        assert!(!leftover, "refused export left {name} or a partial");
    }
}

fn packaged(snap: &Path, rel: &str) -> (u32, Vec<u8>) {
    let file = snap.join(rel);
    let meta = file.symlink_metadata().unwrap();
    assert!(meta.is_file(), "{rel} is not a regular packaged file");
    assert!(
        !meta.file_type().is_symlink(),
        "{rel} packaged as a symlink"
    );
    (meta.mode() & 0o777, fs::read(file).unwrap())
}

fn tracker_rel_paths(snap: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![snap.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = path.symlink_metadata().unwrap();
            if meta.is_dir() && !meta.file_type().is_symlink() {
                stack.push(path);
            } else {
                files.push(path.strip_prefix(snap).unwrap().display().to_string());
            }
        }
    }
    files.sort();
    files
}

fn assert_snapshot_tree(package: &Path) {
    let snap = package.join("tracker");
    assert!(
        !snap.join(".git").exists(),
        "packaged tracker has git metadata"
    );
    assert_eq!(
        tracker_rel_paths(&snap),
        vec![
            ".gitignore".to_string(),
            "edited.sh".to_string(),
            "extra.txt".to_string(),
        ]
    );
    let (ignore_mode, ignore) = packaged(&snap, ".gitignore");
    assert_eq!(ignore, GITIGNORE);
    assert_eq!(ignore_mode, 0o600, "unchanged file mode");
    let (edited_mode, edited) = packaged(&snap, "edited.sh");
    assert_eq!(edited, EDITED_NEW, "edited bytes were not captured");
    assert_eq!(edited_mode, 0o700, "executable edit mode was not captured");
    let (extra_mode, extra) = packaged(&snap, "extra.txt");
    assert_eq!(extra, EXTRA, "untracked bytes were not captured");
    assert_eq!(extra_mode, 0o600, "untracked file mode");
    assert!(!snap.join("gone.txt").exists(), "deletion was captured");
    assert!(
        !snap.join("noise.log").exists(),
        "ignored file was captured"
    );
}

fn assert_no_hook_material(package: &Path) {
    let mut stack = vec![package.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(package).unwrap().display().to_string();
            assert!(
                !rel.split('/').any(|part| part == ".git" || part == "hooks"),
                "hook path published at {rel}"
            );
            let meta = path.symlink_metadata().unwrap();
            assert!(!meta.file_type().is_symlink(), "package symlink at {rel}");
            if meta.is_dir() {
                stack.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                assert!(
                    !bytes
                        .windows(SENTINEL.len())
                        .any(|window| window == SENTINEL),
                    "hook sentinel published at {rel}"
                );
            }
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
                    "{} {:o} {}",
                    path.strip_prefix(root).unwrap().display(),
                    meta.mode() & 0o777,
                    sha256(&fs::read(&path).unwrap())
                ));
            }
        }
    }
    files.sort();
    sha256(files.join("\n").as_bytes())
}

fn assert_ordinary_refuses(
    state: &Path,
    tracker: &Path,
    out: &Path,
    needle: &str,
    before: &Finger,
    marker: &Path,
) {
    let err = export_portable(state, out, &opts(tracker)).expect_err("ordinary export refuses");
    assert!(
        err.to_string().contains(needle),
        "ordinary export refused without {needle}: {err}"
    );
    assert_unpublished(out);
    assert_hooks_did_not_run(marker);
    assert_source_unchanged(tracker, before);
}

fn assert_snapshot_succeeds(
    state: &Path,
    tracker: &Path,
    package: &Path,
    before: &Finger,
    marker: &Path,
) -> String {
    let report = export_portable_snapshot_selected(state, package, &opts(tracker), &[])
        .expect("hook-bearing snapshot exports");
    let rendered = serde_json::to_string(&report).unwrap();
    assert!(
        !rendered
            .as_bytes()
            .windows(SENTINEL.len())
            .any(|window| window == SENTINEL),
        "snapshot report contains the hook sentinel"
    );
    assert_hooks_did_not_run(marker);
    assert_source_unchanged(tracker, before);
    assert_snapshot_tree(package);
    assert_no_hook_material(package);
    let digest = tree_digest(package);
    inspect_portable(package).expect("snapshot package inspects");
    assert_eq!(tree_digest(package), digest, "inspect is read-only");
    assert_hooks_did_not_run(marker);
    assert_source_unchanged(tracker, before);
    rendered
}

#[test]
fn cad1557_hooked_dirty_snapshot_exports_without_running_hooks() {
    let owned = tempfile::Builder::new()
        .prefix("cad1557-hook-acceptance-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = owned.path();
    let state = current_store(root);

    let default_root = root.join("default-hooks");
    fs::create_dir_all(&default_root).unwrap();
    let default_tracker = dirty_tracker(&default_root);
    let default_marker = default_root.join("hook-executed");
    remove_active_hook_entries(&default_tracker);
    install_failing_hooks(
        &default_tracker.join(".git/hooks"),
        &hook_script(&default_marker),
    );
    assert_hooks_did_not_run(&default_marker);
    let default_before = fingerprint(&default_tracker);
    let default_package = default_root.join("snapshot-out");
    assert_snapshot_succeeds(
        &state,
        &default_tracker,
        &default_package,
        &default_before,
        &default_marker,
    );
    assert_ordinary_refuses(
        &state,
        &default_tracker,
        &default_root.join("ordinary-out"),
        "active git hooks",
        &default_before,
        &default_marker,
    );

    let external_root = root.join("external-hooks");
    fs::create_dir_all(&external_root).unwrap();
    let external_tracker = dirty_tracker(&external_root);
    remove_active_hook_entries(&external_tracker);
    let external_hooks = external_root.join("configured-hooks");
    let external_marker = external_root.join("hook-executed");
    install_failing_hooks(&external_hooks, &hook_script(&external_marker));
    let hooks_path = external_hooks.to_str().expect("hooks path is utf-8");
    assert!(!hooks_path.contains('\n') && !hooks_path.contains("hookspath"));
    let mut config = fs::read(external_tracker.join(".git/config")).unwrap();
    config.extend_from_slice(format!("\n[core]\n\thooksPath = {hooks_path}\n").as_bytes());
    fs::write(external_tracker.join(".git/config"), &config).unwrap();
    assert_hooks_did_not_run(&external_marker);
    let external_before = fingerprint(&external_tracker);
    let external_package = external_root.join("snapshot-out");
    let rendered = assert_snapshot_succeeds(
        &state,
        &external_tracker,
        &external_package,
        &external_before,
        &external_marker,
    );
    assert!(
        !rendered.contains(hooks_path),
        "snapshot report contains the configured hooks directory"
    );
    assert_ordinary_refuses(
        &state,
        &external_tracker,
        &external_root.join("ordinary-out"),
        "hookspath",
        &external_before,
        &external_marker,
    );

    let link_root = root.join("symlink-hook");
    fs::create_dir_all(&link_root).unwrap();
    let link_tracker = dirty_tracker(&link_root);
    remove_active_hook_entries(&link_tracker);
    let link_marker = link_root.join("hook-executed");
    let link_target = link_root.join("hook-target.sh");
    write_file(&link_target, &hook_script(&link_marker), 0o755);
    fs::create_dir_all(link_tracker.join(".git/hooks")).unwrap();
    std::os::unix::fs::symlink(&link_target, link_tracker.join(".git/hooks/pre-commit")).unwrap();
    assert_hooks_did_not_run(&link_marker);
    let link_before = fingerprint(&link_tracker);
    let link_out = link_root.join("snapshot-out");
    let err = export_portable_snapshot_selected(&state, &link_out, &opts(&link_tracker), &[])
        .expect_err("symlink hook capture refuses");
    let msg = err.to_string();
    assert!(
        msg.contains("symlink") || msg.contains("active git hooks"),
        "symlink hook guard missing: {msg}"
    );
    assert_unpublished(&link_out);
    assert_hooks_did_not_run(&link_marker);
    assert_source_unchanged(&link_tracker, &link_before);
}
