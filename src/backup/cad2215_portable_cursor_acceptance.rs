//! CAD-2215 independent acceptance (author: PortableCursorAcceptance, not the
//! implementer). One explicitly provisioned synthetic scoped-cursor binding is
//! created in an isolated state directory, sealed into a real job-event
//! cursor, exported and restored through the portable APIs, then read back
//! from both stores.
//!
//! Proven behaviour:
//! * export and restore leave the source `workspace_id`, its persisted
//!   `cursor_keyring_fingerprint`, the source coordination row and the sealed
//!   token exactly as they were;
//! * the imported store mints a different `workspace_id` and carries the
//!   migration's uninitialized (`NULL`) `cursor_keyring_fingerprint`;
//! * the source token never opens under the imported workspace, even when the
//!   identical explicit keyring material is admitted again, and the source
//!   fingerprint does not verify against that imported codec;
//! * the portable package carries neither the keyring record nor any raw key
//!   byte.
//!
//! The fixture daemon is a real `Shared` constructed over the synthetic state
//! directory, and the only writer of the persisted binding is the real
//! `scoped_cursor_initialize` dispatcher entry under the in-process
//! `test_seam::scoped(Asserted::Operator, …)` channel: the operator guard,
//! the empty-parameter contract, the fixed record loader and the fenced
//! `Store::initialize_cursor_keyring` all run unchanged. The codec used for
//! the cursor is the handler read path `Shared::scoped_cursor_codec`, so a
//! missing record or an uninitialized/mismatched fingerprint refuses before
//! this test can seal anything. Fixtures are an owned 0700 tempdir under
//! `/tmp`; no production state, key, daemon or network is touched, and the
//! module is compiled only for debug test binaries with `test-seam` (the same
//! gate as the daemon's CAD-2215 acceptance).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::backup::portable::{
    export_portable, export_portable_snapshot_selected, inspect_portable, restore_inactive,
    PortableExportOptions, PortableRestoreOptions, Scope,
};
use crate::daemon::scoped_cursor::{CursorBinding, CursorDirection, ScopedCursorCodec};
use crate::daemon::{ServeOptions, Shared};
use crate::delivery::private::{self as private_delivery, VerifiedJobOrigin};
use crate::issue::{write as issue_write, Pm};
use crate::platform::scoped_cursor_config as record;
use crate::rollout::db_file;
use crate::store::{ConversationKind, NewAgent, Store};
use crate::test_seam::Asserted;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const JOB_ID: &str = "cad2215-job";
const JOB_TITLE: &str = "Synthetic portable job";

/// Deterministic synthetic material: fixture input, never production key
/// bytes. Each seed yields a distinct 32-byte block with no long uniform run,
/// so a package byte scan cannot match by accident.
fn synthetic_key(seed: u8) -> [u8; 32] {
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = seed
            .wrapping_mul(37)
            .wrapping_add((index as u8).wrapping_mul(97))
            .wrapping_add(0x5b);
    }
    key
}

fn synthetic_id(seed: u8) -> [u8; 16] {
    let mut id = [0u8; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = seed
            .wrapping_mul(53)
            .wrapping_add((index as u8).wrapping_mul(29))
            .wrapping_add(0x17);
    }
    id
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn owned_dir(path: &Path) {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
}

fn write_owner_file(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
    file.write_all(bytes)
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
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
        .env("GIT_AUTHOR_NAME", "cad2215-acceptance")
        .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad2215-acceptance")
        .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A clean tracker: one commit holding one ticket file. No `pm.yaml`, so the
/// package records no PM root; the tracker only has to pass real admission.
fn tracker_fixture(root: &Path) -> PathBuf {
    let tracker = root.join("tracker");
    fs::create_dir_all(tracker.join("tickets")).unwrap();
    git(&tracker, &["init", "-q"]);
    fs::write(
        tracker.join("tickets/CAD-2215.md"),
        "# CAD-2215 synthetic portable acceptance fixture\n",
    )
    .unwrap();
    git(&tracker, &["add", "-A"]);
    git(&tracker, &["commit", "-q", "-m", "CAD-2215 fixture"]);
    tracker
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn collect_files(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            collect_files(&path, out);
        } else {
            let bytes = fs::read(&path).unwrap();
            out.push((path, bytes));
        }
    }
}

#[test]
fn cad2215_portable_import_cannot_reuse_source_cursor_binding() {
    let owned = tempfile::Builder::new()
        .prefix("cad2215-portable-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = owned.path().to_path_buf();
    // tempfile creates with the process default mode; the fixed record loader
    // requires an owner-only chain below the sticky /tmp.
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("the fixture root is owner-only");
    let tracker = tracker_fixture(&root);

    // Synthetic keys and the fixed record live outside both the state
    // directory and the tracker, which the real loader enforces.
    let keys = root.join("keys");
    owned_dir(&keys);
    let active_key = synthetic_key(0x11);
    let retained_key = synthetic_key(0x42);
    let identity_key = synthetic_key(0x9d);
    let active_id = synthetic_id(0x21);
    let retained_id = synthetic_id(0x64);
    let active_path = keys.join("active.key");
    let retained_path = keys.join("retained.key");
    let identity_path = keys.join("identity.key");
    write_owner_file(&active_path, &active_key);
    write_owner_file(&retained_path, &retained_key);
    write_owner_file(&identity_path, &identity_key);

    let state = root.join("state");
    owned_dir(&state);
    let record_path = state.join("scoped-cursors.json");
    let record_bytes = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "identity_key_file": identity_path,
        "active_key_id": hex(&active_id),
        "keys": [
            {"id": hex(&active_id), "key_file": active_path},
            {"id": hex(&retained_id), "key_file": retained_path},
        ],
    }))
    .unwrap();
    write_owner_file(&record_path, &record_bytes);

    // A real fixture daemon over this state dir. The operator RPC below reads
    // the fixed record and binds its fingerprint; `Shared::new` itself never
    // touches the keyring.
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", state.join("tracker").to_str().unwrap());
    let shared = Shared::new(&state, &opts).expect("the fixture daemon opens");
    let store = shared.store.clone();
    let (source_workspace, uninitialized) =
        store.cursor_keyring_metadata().expect("workspace metadata");
    assert!(
        uninitialized.is_none(),
        "a fresh store has never bound a cursor keyring"
    );

    // The one writer: the proven-operator dispatcher entry with the empty
    // parameter contract. Nothing is written before the guard passes.
    let initialized = crate::test_seam::scoped(Asserted::Operator, || {
        shared.dispatch(
            "scoped_cursor_initialize",
            &serde_json::json!({}),
            std::process::id(),
        )
    })
    .expect("the proven operator initializes the keyring");
    assert_eq!(
        initialized["initialized"],
        serde_json::json!(true),
        "{initialized}"
    );
    let source_fingerprint = initialized["fingerprint"]
        .as_str()
        .expect("the operator answer names the fingerprint")
        .to_string();
    let (bound_workspace, bound) = store.cursor_keyring_metadata().unwrap();
    assert_eq!(bound_workspace, source_workspace);
    assert_eq!(bound.as_deref(), Some(source_fingerprint.as_str()));

    // The handler read path loads the record and proves the persisted
    // fingerprint before handing out the sealing codec.
    let codec = shared
        .scoped_cursor_codec()
        .expect("the initialized ring is admitted");
    assert_eq!(codec.keyring_fingerprint(), source_fingerprint.as_str());

    // One real coordination row, carried by the portable profile.
    store
        .register_agent(&NewAgent {
            alias: "cad2215pm",
            provider: "codex",
            endpoint_kind: "managed",
            role: "pm",
            cwd: root.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("fixture agent registers");
    let (duplicate, job) = store
        .create_job(
            JOB_ID,
            Some(JOB_TITLE),
            "spec.md",
            &"a".repeat(64),
            "cad2215pm",
            None,
            None,
            None,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("fixture job is created");
    assert!(!duplicate);
    assert_eq!(job.id, JOB_ID);

    let binding = CursorBinding {
        domain: "job-events",
        direction: CursorDirection::Forward,
        principal: "principal-cad2215",
        resource: JOB_ID,
        filters: "after=41",
    };
    let token = codec.seal(41, &binding).expect("real cursor seal");
    assert_eq!(
        codec
            .open(&token, &binding)
            .expect("source namespace opens"),
        41
    );

    let package = root.join("package");
    export_portable(
        &state,
        &package,
        &PortableExportOptions {
            scope: Scope::Coordination,
            tracker: tracker.clone(),
            redact_history_secrets: false,
            attest_chat_custody_absent: true,
        },
    )
    .expect("the provisioned source exports");
    assert!(package.join("manifest.json").is_file(), "package published");

    // Neither the fixed record nor any raw key byte may travel in a package.
    let mut members = Vec::new();
    collect_files(&package, &mut members);
    assert!(
        !members.iter().any(|(path, _)| {
            path.file_name().and_then(|name| name.to_str()) == Some("scoped-cursors.json")
        }),
        "the keyring record must not be exported"
    );
    for (path, bytes) in &members {
        for (label, key) in [
            ("active", &active_key),
            ("retained", &retained_key),
            ("identity", &identity_key),
        ] {
            assert!(
                !contains_bytes(bytes, key),
                "{} carries the {label} key bytes",
                path.display()
            );
        }
        assert!(
            !contains_bytes(bytes, source_fingerprint.as_bytes()),
            "{} carries the derived keyring fingerprint",
            path.display()
        );
    }

    let destination = root.join("restored");
    restore_inactive(
        &package,
        &destination,
        &PortableRestoreOptions { replace: false },
    )
    .expect("the package restores into a new directory");

    // The source is untouched by export or restore.
    let (after_workspace, after_fingerprint) = store
        .cursor_keyring_metadata()
        .expect("source metadata still readable");
    assert_eq!(after_workspace, source_workspace, "source workspace intact");
    assert_eq!(
        after_fingerprint.as_deref(),
        Some(source_fingerprint.as_str()),
        "source cursor binding intact"
    );
    assert_eq!(
        store
            .job(JOB_ID)
            .expect("source row intact")
            .title
            .as_deref(),
        Some(JOB_TITLE)
    );
    assert_eq!(
        codec
            .open(&token, &binding)
            .expect("source token still opens"),
        41
    );
    assert_eq!(
        fs::read(&record_path).expect("source record intact"),
        record_bytes,
        "the fixed keyring record is not rewritten"
    );

    // The receiver mints its own workspace and leaves the cursor binding
    // uninitialized: no source authority is inherited.
    let imported = Store::open(&db_file(&destination)).expect("imported store opens");
    let (imported_workspace, imported_fingerprint) = imported
        .cursor_keyring_metadata()
        .expect("imported metadata");
    assert!(!imported_workspace.is_empty());
    assert_ne!(
        imported_workspace, source_workspace,
        "an import must not reuse the source workspace identity"
    );
    assert!(
        imported_fingerprint.is_none(),
        "an import must not carry the source cursor-key fingerprint"
    );
    assert_eq!(
        imported.job(JOB_ID).expect("coordination row imported").id,
        JOB_ID
    );
    assert!(
        record::load(&destination, Some(tracker.as_path()))
            .expect("the imported state is readable")
            .is_none(),
        "the imported state carries no keyring record"
    );

    // The same admitted key material cannot make the imported namespace open
    // the source token, and the source fingerprint does not verify there.
    let replay = record::load(&state, Some(tracker.as_path()))
        .expect("the same admitted record loads")
        .expect("the record is present");
    let imported_codec = ScopedCursorCodec::new(
        &imported_workspace,
        replay.keyring,
        replay.active_key_id,
        &replay.identity_key[..],
    )
    .expect("the same keys build a codec for the imported namespace");
    assert_ne!(
        imported_codec.keyring_fingerprint(),
        source_fingerprint.as_str(),
        "the fingerprint is bound to the workspace namespace"
    );
    assert!(
        imported_codec.open(&token, &binding).is_err(),
        "the source token must not open in the imported workspace"
    );
    assert!(
        imported_codec
            .verify_keyring_fingerprint(&source_fingerprint)
            .is_err(),
        "the source fingerprint must not verify against the imported namespace"
    );
}

// ---------- CAD-2215: a real export refuses private member work ----------
//
// Independent acceptance (author: PrivacyAcceptance, not the implementer) for
// the other half of this slice. The clean workspace above still exports and
// restores with its cursor-namespace semantics; these tests prove that a
// workspace holding member-private work is REFUSED by the real
// `export_portable` entry point instead of leaking it or silently dropping the
// provenance.
//
// Covered: a queued-but-unpromoted request; a completed member-private job
// (ownership ledger); an app conversation with no grant; a revoked grant
// tombstone; the private delivery ledger; a publication comment in the
// tracker; a private project policy; an unclassifiable per-install
// `app-records/<install>.sqlite3`; a human-room transcript; a workspace chat
// file whose `chat_files` index the profile omits; a retained cloud-agent
// session; a private project policy inside an externally produced package at
// inspect/restore (built by re-hashing one member of a real export, not by
// reproducing the guard), including the same package with a null or
// mismatched manifest `pm_root`; a project-shaped private policy in a tracker
// with no `pm.yaml`; an orphan daemon `assist_jobs` spec; an orphan
// PM chat-custody blob under `.cadence/chat-files` with no `chat_files` row;
// a `chat_upload_accounts` row; package no-follow cases across inspect and
// restore (symlinked member leaf, symlinked member ancestor, non-regular FIFO
// store leaf, hardlinked member); and untrusted-package resource bounds (a
// 1 TiB sparse tracker member over its declared size, and an 8 MiB sparse
// manifest) at inspect and restore.
//
// A deterministic in-flight swap is NOT covered: it would need an injected
// callback between the package traversal and the member read, and portable.rs
// has no such seam. The guard is now descriptor-pinned (`PackageRoot` opens
// every component with O_NOFOLLOW and fstats the leaf; `read_member` checks
// the opened inode's size and reads at most the declared bound), so a swap
// after the path resolves cannot change the inode being read. The static
// cases above are the reachable half.
//
// The daemon's separate operational workspace
// (`<sandbox_home>/workspace/users/...`) is asserted at its strongest
// reachable point: a verified pre-upload backup, a real upload account and
// operational blob in a daemon-resolved sandbox workspace, then
// `restore --force` of the earlier DB, then export must refuse with the blob
// intact. That acceptance is RED today: `export_portable*` receive only the
// state dir and the tracker, with no durable workspace binding, so they
// cannot locate the root. The account row alone is not proof — the restore
// erases it while the blob stays. Closing it needs a persisted,
// export-readable workspace binding (or an explicit export workspace).
//
// Every case starts with a clean export, which is the positive control that
// keeps a blanket refusal from passing. That control now carries the explicit
// `attest_chat_custody_absent` statement the exporter requires of an
// otherwise-clean source (the operational custody root cannot be located from
// the store), so a case that needs the clean export to succeed asserts the
// operator's fact rather than weakening the default. The default itself is
// guarded by `cad2215_clean_source_refuses_without_a_custody_attestation`, and
// a classifiable private record still refuses with the attestation set.
//
// Assertions are behavioural only — refusal, no package, no staging, and the
// private source record still intact — never the refusal's wording.

const ORG: &str = "cad2215-portable-org";

fn refusal_root(name: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("cad2215-refuse-{name}-"))
        .tempdir_in("/tmp")
        .expect("an isolated refusal fixture dir");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
        .expect("the refusal fixture root is owner-only");
    dir
}

fn state_with_store(root: &Path) -> PathBuf {
    let state = root.join("state");
    owned_dir(&state);
    drop(Store::open(&db_file(&state)).expect("the fixture store opens"));
    state
}

fn store_at(state: &Path) -> Store {
    Store::open(&db_file(state)).expect("the fixture store reopens")
}

fn export_options(tracker: &Path) -> PortableExportOptions {
    PortableExportOptions {
        scope: Scope::Coordination,
        tracker: tracker.to_path_buf(),
        redact_history_secrets: false,
        attest_chat_custody_absent: true,
    }
}

/// The same workspace, before the private row exists, must export: this is
/// the positive control that makes every refusal below meaningful.
fn clean_export(state: &Path, tracker: &Path, out: &Path) {
    export_portable(state, out, &export_options(tracker)).expect("the clean fixture exports");
    assert!(
        out.join("manifest.json").is_file(),
        "the clean export published no manifest"
    );
}

/// Run the real exporter over a workspace that now holds private work and
/// assert only the behaviour: it refuses, and it publishes nothing (no
/// package, no staging leftover). The refusal's wording is not pinned; each
/// caller separately proves the private source record is still intact.
fn refuse(state: &Path, tracker: &Path, out: &Path) {
    export_portable(state, out, &export_options(tracker))
        .expect_err("a workspace holding private work exported");
    assert!(
        !out.exists(),
        "a refused export published {}",
        out.display()
    );
    let staging = out
        .parent()
        .expect("the fixture output has a parent")
        .join(format!(
            ".{}.partial-{}",
            out.file_name()
                .expect("the output has a name")
                .to_string_lossy(),
            std::process::id()
        ));
    assert!(
        !staging.exists(),
        "a refused export left its staging dir behind"
    );
}

/// The default is fail-closed even for a clean source: the exporter cannot
/// locate the operational chat-attachment custody root, so the absence the
/// package relies on has to be an explicit operator statement for that source.
/// Without this guard the clean control in every other test would erase the
/// only proof that the default still refuses.
#[test]
fn cad2215_clean_source_refuses_without_a_custody_attestation() {
    let root = refusal_root("no-attestation");
    let state = state_with_store(root.path());
    let pm = native_tracker(root.path());
    let out = root.path().join("package");
    let mut opts = export_options(pm.dir.as_path());
    opts.attest_chat_custody_absent = false;
    let error = export_portable(&state, &out, &opts)
        .expect_err("a clean source exported without the operator's custody statement");
    assert!(
        !out.exists(),
        "a refused export published {}",
        out.display()
    );
    assert!(
        format!("{error}").contains("attest-chat-custody-absent"),
        "the refusal must name the flag that admits it: {error}"
    );
}

/// A real native PM tracker with one project, the same shape the clean
/// portable selftest exports, so tracker-side classification is exercised on
/// committed content.
fn native_tracker(root: &Path) -> Pm {
    let pm = Pm::init(&root.join("tracker")).expect("native Pm::init");
    crate::issue::hooks::install(&pm.dir).expect("native issue hooks");
    issue_write::project_add(&pm, "cad", "CAD", &[], &[], &[], None).expect("real project add");
    pm
}

fn commit_tracker(tracker: &Path, message: &str) {
    git(tracker, &["add", "-A"]);
    git(tracker, &["commit", "-q", "-m", message]);
}

/// Re-hash one packaged member after its bytes changed, so the package stays
/// internally consistent. Packaging mechanics only — no guard is reproduced.
fn rehash_manifest_member(package: &Path, member_path: &str, bytes: &[u8]) {
    let manifest_path = package.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("the manifest"))
            .expect("the manifest decodes");
    let digest = format!("{:x}", Sha256::digest(bytes));
    let mut updated = false;
    for member in manifest["members"]
        .as_array_mut()
        .expect("manifest members")
    {
        if member["path"] == json!(member_path) {
            member["sha256"] = json!(digest);
            member["bytes"] = json!(bytes.len());
            updated = true;
        }
    }
    assert!(updated, "the package manifest does not list {member_path}");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("the manifest serializes"),
    )
    .expect("the rewritten manifest");
}

/// Replace a package's manifest `pm_root`, keeping every member digest valid.
fn set_manifest_pm_root(package: &Path, value: Value) {
    let manifest_path = package.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("the manifest"))
            .expect("the manifest decodes");
    manifest["pm_root"] = value;
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("the manifest serializes"),
    )
    .expect("the rewritten manifest");
}

/// Rewrite the package's bundled `project.yaml` to a private policy and
/// re-hash that one member. Returns the packaged record path and its bytes.
fn rewrite_packaged_policy(package: &Path) -> (PathBuf, Vec<u8>) {
    let record_path = package.join("tracker").join("cad").join("project.yaml");
    let mut project: crate::issue::project::Project =
        serde_yaml::from_slice(&fs::read(&record_path).expect("the packaged project record"))
            .expect("the packaged project record decodes");
    let mut policy = project.policy.clone().unwrap_or_default();
    policy.version = policy.version.saturating_add(1);
    policy.visibility = crate::issue::project::Visibility::Private;
    project.policy = Some(policy);
    let text = serde_yaml::to_string(&project).expect("the private policy serializes");
    let bytes = text.into_bytes();
    fs::write(&record_path, &bytes).expect("the packaged project record");
    rehash_manifest_member(package, "tracker/cad/project.yaml", &bytes);
    (record_path, bytes)
}

/// Drop one member from the package's manifest so it matches a package that no
/// longer carries that file.
fn remove_manifest_member(package: &Path, member_path: &str) {
    let manifest_path = package.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("the manifest"))
            .expect("the manifest decodes");
    let members = manifest["members"]
        .as_array_mut()
        .expect("manifest members");
    let before = members.len();
    members.retain(|member| member["path"] != json!(member_path));
    assert_eq!(
        members.len() + 1,
        before,
        "the package manifest does not list {member_path}"
    );
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("the manifest serializes"),
    )
    .expect("the rewritten manifest");
}

/// A real exported package whose bundled tracker records a private project
/// policy. `drop_pm_yaml` removes the packaged `pm.yaml` member as well, so
/// the manifest and the tracker agree that the package has no PM root.
struct PolicyPackage {
    _root: tempfile::TempDir,
    package: PathBuf,
    record: PathBuf,
    bytes: Vec<u8>,
}

fn private_policy_package(name: &str, drop_pm_yaml: bool) -> PolicyPackage {
    let root = refusal_root(name);
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let package = root.path().join("package");
    clean_export(&state, &pm.dir, &package);
    let (record, bytes) = rewrite_packaged_policy(&package);
    if drop_pm_yaml {
        fs::remove_file(package.join("tracker").join("pm.yaml")).expect("the packaged pm record");
        remove_manifest_member(&package, "tracker/pm.yaml");
    }
    PolicyPackage {
        _root: root,
        package,
        record,
        bytes,
    }
}

fn assert_refused_restore(package: &Path, destination: &Path) {
    assert!(
        restore_inactive(
            package,
            destination,
            &PortableRestoreOptions { replace: false },
        )
        .is_err(),
        "restore accepted {}",
        package.display()
    );
    assert!(
        !destination.exists(),
        "a refused restore published {}",
        destination.display()
    );
}

#[test]
fn cad2215_portable_refuses_a_queued_member_request() {
    let root = refusal_root("queued");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    let store = store_at(&state);
    store
        .assist_request_enqueue(
            ORG,
            "member-a",
            "key-queued",
            "CAD-2215",
            "PRIVATE_QUEUED_TITLE",
            "PRIVATE_QUEUED_BODY",
            "PRIVATE_QUEUED_ACCEPTANCE",
            "cred-cad2215",
        )
        .expect("a real queued member request");
    assert!(
        store
            .assist_request_queued(ORG, "member-a", "key-queued")
            .expect("the queued row reads back")
            .is_some(),
        "the fixture queued request did not persist"
    );

    refuse(&state, &tracker, &root.path().join("refused"));

    let after = store_at(&state);
    let queued = after
        .assist_request_queued(ORG, "member-a", "key-queued")
        .expect("the source queue still reads")
        .expect("the refused export removed the queued request");
    assert_eq!(queued.issue, "CAD-2215");
    assert_eq!(queued.body, "PRIVATE_QUEUED_BODY");
    assert!(
        !queued.completed,
        "the refused export completed the request"
    );
}

#[test]
fn cad2215_portable_refuses_completed_member_private_work() {
    let root = refusal_root("completed");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    let store = store_at(&state);
    store
        .register_agent(&NewAgent {
            alias: "cad2215pm",
            provider: "codex",
            endpoint_kind: "managed",
            role: "pm",
            cwd: root.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("the fixture PM registers");
    store
        .assist_request_enqueue(
            ORG,
            "member-a",
            "key-completed",
            "CAD-2215",
            "PRIVATE_JOB_TITLE",
            "PRIVATE_JOB_BODY",
            "PRIVATE_JOB_ACCEPTANCE",
            "cred-cad2215",
        )
        .expect("a real queued request");
    // The queue row names the job the promotion will mint; create exactly that
    // job so the completed fixture is one coherent request, not a row pair.
    let planned = store
        .assist_request_queued(ORG, "member-a", "key-completed")
        .expect("the queued row reads back")
        .expect("the queued request")
        .planned_job_id;
    let (duplicate, job) = store
        .create_job(
            &planned,
            Some("PRIVATE_JOB_TITLE"),
            "spec.md",
            &"a".repeat(64),
            "cad2215pm",
            None,
            None,
            None,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("the fixture private job is created");
    assert!(!duplicate, "the planned private job id was already taken");
    assert_eq!(job.id, planned);
    assert_eq!(
        store
            .assist_request_bind(
                ORG,
                "member-a",
                "key-completed",
                &job.id,
                "PRIVATE_JOB_TITLE",
                "PRIVATE_JOB_BODY",
                "PRIVATE_JOB_ACCEPTANCE",
                "cred-cad2215",
            )
            .expect("the real ledger bind"),
        job.id
    );
    store
        .assist_request_complete(ORG, "member-a", "key-completed")
        .expect("the request is marked finished");
    assert!(
        store
            .assist_request_queued(ORG, "member-a", "key-completed")
            .expect("the completed row reads back")
            .expect("the completed request")
            .completed,
        "the fixture request is not a completed one"
    );

    refuse(&state, &tracker, &root.path().join("refused"));

    let after = store_at(&state);
    let recorded = after
        .assist_request_record(ORG, "member-a", "key-completed")
        .expect("the source ledger still reads")
        .expect("the refused export removed the member's private job provenance");
    assert_eq!(recorded.job, job.id);
    assert_eq!(recorded.body.as_deref(), Some("PRIVATE_JOB_BODY"));
    assert_eq!(
        after.job(&job.id).expect("the source job still reads").id,
        job.id,
        "the refused export removed the private job row"
    );
}

#[test]
fn cad2215_portable_refuses_an_app_conversation_without_a_grant() {
    let root = refusal_root("conversation");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    let store = store_at(&state);
    store
        .register_agent(&NewAgent {
            alias: "master",
            provider: "fake",
            endpoint_kind: "fake",
            role: "pm",
            cwd: root.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("the fixture app owner registers");
    let (thread, created) = store
        .conversation_create("master", "inst_cad2215app", None, ConversationKind::General)
        .expect("a real app conversation");
    assert!(created, "the fixture app conversation was not created");

    refuse(&state, &tracker, &root.path().join("refused"));

    assert!(
        store_at(&state)
            .thread_by_id(&thread.id)
            .expect("the source conversation still reads")
            .is_some(),
        "the refused export removed the app conversation"
    );
}

#[test]
fn cad2215_portable_refuses_a_revoked_grant_tombstone() {
    let root = refusal_root("grant");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    let store = store_at(&state);
    let digest = format!("sha256:{}", "b".repeat(64));
    store
        .app_member_grant_set(
            ORG,
            "member-a",
            "inst_cad2215grant",
            "conv_cad2215grant",
            None,
            &digest,
            "operator",
        )
        .expect("a real active grant");
    let revoked = store
        .app_member_grant_revoke(
            ORG,
            "member-a",
            "inst_cad2215grant",
            "conv_cad2215grant",
            "operator",
        )
        .expect("the real revoke")
        .expect("the grant tombstone");
    assert_eq!(revoked.state, "revoked");

    refuse(&state, &tracker, &root.path().join("refused"));

    let rows = store_at(&state)
        .app_member_grants_list(ORG, "inst_cad2215grant")
        .expect("the source grants still read");
    assert_eq!(rows.len(), 1, "the refused export changed the grant rows");
    assert_eq!(
        rows[0].state, "revoked",
        "the tombstone was dropped or revived"
    );
    assert_eq!(rows[0].conversation_id, "conv_cad2215grant");
}

#[test]
fn cad2215_portable_refuses_the_private_delivery_ledger() {
    let root = refusal_root("delivery");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    private_delivery::open(
        &state,
        VerifiedJobOrigin {
            job: "cad2215-delivery-job".into(),
            task: "cad2215-delivery-task".into(),
            revision: 1,
            dispatch: "cad2215-delivery-dispatch".into(),
            issue: "CAD-2215".into(),
            project: "cad".into(),
            owner_org: ORG.into(),
            owner_sub: "member-a".into(),
            authority_rev: Some("1.0".into()),
        },
        1,
    )
    .expect("the real private delivery writer records one delivery");
    let ledger = private_delivery::path(&state);
    let before = fs::read(&ledger).expect("the private ledger file");

    refuse(&state, &tracker, &root.path().join("refused"));

    assert_eq!(
        fs::read(&ledger).expect("the private ledger is still readable"),
        before,
        "the refused export rewrote the private delivery ledger"
    );
    assert_eq!(
        private_delivery::load(&state)
            .expect("the private ledger still reads")
            .len(),
        1,
        "the refused export removed the private delivery receipt"
    );
}

#[test]
fn cad2215_portable_refuses_a_publication_comment() {
    let root = refusal_root("publication");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let created = issue_write::new_issue(
        &pm,
        root.path(),
        Some("cad"),
        "PRIVATE_PUBLICATION_TITLE",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        Some("PRIVATE_PUBLICATION_BODY"),
        "cad2215-acceptance",
    )
    .expect("a real ticket in the fixture tracker");
    let id = created["id"]
        .as_str()
        .expect("the created ticket id")
        .to_string();
    clean_export(&state, &pm.dir, &root.path().join("clean"));

    let comments = pm.dir.join("cad").join(&id).join("comments");
    fs::create_dir_all(&comments).expect("the ticket comments dir");
    let publication = comments.join("2026-01-01T00:00:00Z-publication.md");
    let body = "---\nkind: publication\n---\nPublication-Attempt: 1\n\
                PRIVATE_PUBLICATION_BODY\n";
    fs::write(&publication, body).expect("the publication comment");
    commit_tracker(&pm.dir, "CAD-2215 publication fixture");

    refuse(&state, &pm.dir, &root.path().join("refused"));

    assert_eq!(
        fs::read_to_string(&publication).expect("the publication comment is still readable"),
        body,
        "the refused export rewrote the publication comment"
    );
}

#[test]
fn cad2215_portable_refuses_a_private_project_policy() {
    let root = refusal_root("policy");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &pm.dir, &root.path().join("clean"));

    // The shipping policy writer deliberately refuses `private` while
    // `private_available()` is false, so this fixture records the policy the
    // same way an operator's reviewed workspace holds it: a real, decodable
    // `project.yaml` whose policy names private visibility.
    let record_path = pm.dir.join("cad").join("project.yaml");
    let mut project: crate::issue::project::Project =
        serde_yaml::from_slice(&fs::read(&record_path).expect("the project record"))
            .expect("the fixture project record decodes");
    let mut policy = project.policy.clone().unwrap_or_default();
    policy.version = policy.version.saturating_add(1);
    policy.visibility = crate::issue::project::Visibility::Private;
    project.policy = Some(policy);
    let private_record = serde_yaml::to_string(&project).expect("the private policy serializes");
    fs::write(&record_path, &private_record).expect("the private policy record");
    commit_tracker(&pm.dir, "CAD-2215 private policy fixture");

    refuse(&state, &pm.dir, &root.path().join("refused"));

    assert_eq!(
        fs::read_to_string(&record_path).expect("the project record is still readable"),
        private_record,
        "the refused export rewrote the project policy"
    );
}

#[test]
fn cad2215_portable_refuses_an_unclassifiable_app_record_file() {
    use crate::store::app_records::{record_db_path, CustomerProfile, RecordStore};

    let root = refusal_root("app-records");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    // A per-install record file is not a package member and this fixture
    // gives the store no app conversation, grant or context to classify it
    // with: the file alone can hold member-private work, so the exporter must
    // not silently leave it behind while publishing the surrounding
    // coordination data.
    let profile = CustomerProfile::parse(&json!({
        "schema": 1,
        "display_name": "PRIVATE_RECORD_MEMBER",
        "email": "private.record@example.invalid",
        "tags": [],
        "consent": {"email": "granted"}
    }))
    .expect("the fixture record profile parses");
    let records =
        RecordStore::open(&state, "inst-cad2215records").expect("a real per-install record store");
    records
        .app_record_create("ctx-cad2215records", "record-private-member", &profile)
        .expect("a real record in the per-install store");
    let record_file =
        record_db_path(&state, "inst-cad2215records").expect("the record path is valid");
    assert!(
        record_file.is_file(),
        "the fixture record file was not created"
    );

    refuse(&state, &tracker, &root.path().join("refused"));

    let reopened = RecordStore::open(&state, "inst-cad2215records")
        .expect("the source record store still opens");
    let stored = reopened
        .app_record_show("ctx-cad2215records", "record-private-member")
        .expect("the refused export removed the private record");
    assert_eq!(
        stored["record"]["profile"]["display_name"],
        "PRIVATE_RECORD_MEMBER"
    );
    assert!(
        record_file.is_file(),
        "the refused export removed the record file"
    );

    // The directory alone is not private state: once its records are gone an
    // empty one holds nothing, so the same workspace must export again.
    drop(reopened);
    drop(records);
    for entry in fs::read_dir(state.join("app-records")).expect("the fixture record dir") {
        let entry = entry.expect("a fixture record dir entry");
        fs::remove_file(entry.path()).expect("remove the fixture record file");
    }
    export_portable(
        &state,
        &root.path().join("emptied"),
        &export_options(&tracker),
    )
    .expect("an empty app-records directory must not block the export");
}

#[test]
fn cad2215_portable_refuses_a_human_room_transcript() {
    use crate::store::human_threads::VerifiedHuman;

    let root = refusal_root("human-room");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    // A human room's transcript is private person-to-person conversation:
    // the coordination profile currently drops those rows outright, which
    // would publish everything around them and silently discard the content.
    let member = VerifiedHuman::with_name("https://cad2215.example", ORG, "member-a", "Member A")
        .expect("a verified fixture human");
    let store = store_at(&state);
    let room = store
        .human_group_create(
            &member,
            "human_group",
            "Private room",
            "cad2215-private-room",
            &[],
        )
        .expect("a real human room");
    store
        .human_room_post(&member, &room, "client-cad2215", "PRIVATE_HUMAN_TRANSCRIPT")
        .expect("a real human-room transcript post");

    refuse(&state, &tracker, &root.path().join("refused"));

    let history = store_at(&state)
        .human_room_history(&member, &room, None, 10)
        .expect("the source transcript still reads");
    assert!(
        history
            .iter()
            .any(|post| post.body == "PRIVATE_HUMAN_TRANSCRIPT"),
        "the refused export removed the human-room transcript"
    );
}

#[test]
fn cad2215_portable_refuses_an_unclassifiable_chat_file() {
    let root = refusal_root("chat-file");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    // One workspace attachment with no assist or app-grant marker: the
    // coordination profile omits the `chat_files` index while the surrounding
    // message stays, so the file must not be silently left behind.
    let uploads = state.join(crate::wiki::UPLOAD_DIR);
    fs::create_dir_all(&uploads).expect("the upload staging dir");
    let staged = uploads.join("cad2215-private.txt");
    fs::write(&staged, "PRIVATE_CHAT_FILE_BODY\n").expect("the staged upload");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).expect("the workspace dir");
    let file = store_at(&state)
        .chat_file_put_in_workspace(
            crate::store::ChatFileStorageRoots {
                state_dir: state.as_path(),
                workspace_dir: workspace.as_path(),
            },
            &staged,
            "cad2215-private.txt",
            "home",
            "",
            "operator",
        )
        .expect("a real workspace chat file");

    refuse(&state, &tracker, &root.path().join("refused"));

    assert!(
        store_at(&state)
            .chat_file(&file.id)
            .expect("the chat-file row still reads")
            .is_some(),
        "the refused export removed the chat-file row"
    );
    assert!(
        workspace
            .join(".cadence")
            .join("chat-files")
            .join(&file.sha256)
            .is_file(),
        "the refused export removed the chat-file bytes"
    );
}

#[test]
fn cad2215_portable_refuses_cloud_agent_private_state() {
    use crate::store::cloud_agent::Owner;

    let root = refusal_root("cloud-agent");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    // A retained cloud-agent session names the exact person and provider
    // conversation; the coordination profile projects that authority and
    // the cloud rows away, so its presence must refuse rather than lose it.
    let owner = Owner::new("https://cad2215.example", "member-a");
    let session = store_at(&state)
        .cloud_agent_bind_session(
            "cld-cad2215cloud",
            "chatgpt",
            "source-uid-cad2215",
            1,
            &owner,
            "thread-cad2215cloud",
            "conversation-cad2215cloud",
            "remote-session-cad2215",
            None,
            None,
        )
        .expect("a real cloud-agent session");

    refuse(&state, &tracker, &root.path().join("refused"));

    let sessions = store_at(&state)
        .cloud_agent_sessions("cld-cad2215cloud")
        .expect("the cloud sessions still read");
    assert!(
        sessions.iter().any(|row| row.id == session.id),
        "the refused export removed the cloud-agent session"
    );
}

#[test]
fn cad2215_portable_inspect_and_restore_refuse_a_packaged_private_policy() {
    let root = refusal_root("packaged-policy");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let package = root.path().join("package");
    clean_export(&state, &pm.dir, &package);

    // An externally produced package this build did not export: its bundled
    // tracker records a private project policy. Only the one changed member
    // digest is recomputed, so the package stays internally consistent and
    // nothing here reproduces the guard.
    let (record_path, bytes) = rewrite_packaged_policy(&package);

    assert!(
        inspect_portable(&package).is_err(),
        "inspect accepted a package whose tracker records a private project policy"
    );
    let destination = root.path().join("restored");
    assert!(
        restore_inactive(
            &package,
            &destination,
            &PortableRestoreOptions { replace: false },
        )
        .is_err(),
        "restore accepted a package whose tracker records a private project policy"
    );
    assert!(
        !destination.exists(),
        "a refused restore published a destination"
    );
    assert_eq!(
        fs::read(&record_path).expect("the packaged record still reads"),
        bytes,
        "the refused restore rewrote the packaged project record"
    );
}

#[test]
fn cad2215_portable_package_refuses_a_private_policy_under_a_consistent_null_pm_root() {
    // Neither the manifest nor the packaged tracker claims a PM root, so the
    // package passes the root-consistency check; the project-shaped record it
    // still carries is private and must be refused.
    let built = private_policy_package("pm-root-null-consistent", true);
    set_manifest_pm_root(&built.package, Value::Null);
    assert!(
        inspect_portable(&built.package).is_err(),
        "inspect accepted a private project policy under a consistent null pm_root"
    );
    assert_refused_restore(&built.package, &built._root.path().join("restored"));
    assert_eq!(
        fs::read(&built.record).expect("the packaged record still reads"),
        built.bytes,
        "a refused inspect/restore rewrote the packaged project record"
    );
}

#[test]
fn cad2215_portable_package_refuses_a_private_policy_under_a_null_pm_root() {
    let built = private_policy_package("pm-root-null", false);
    set_manifest_pm_root(&built.package, Value::Null);
    assert!(
        inspect_portable(&built.package).is_err(),
        "inspect accepted a private project policy under a null pm_root"
    );
    assert_refused_restore(&built.package, &built._root.path().join("restored"));
    assert_eq!(
        fs::read(&built.record).expect("the packaged record still reads"),
        built.bytes,
        "a refused inspect/restore rewrote the packaged project record"
    );
}

#[test]
fn cad2215_portable_package_refuses_a_private_policy_under_a_mismatched_pm_root() {
    let built = private_policy_package("pm-root-mismatched", false);
    set_manifest_pm_root(&built.package, json!("pm"));
    assert!(
        inspect_portable(&built.package).is_err(),
        "inspect accepted a private project policy under a mismatched pm_root"
    );
    assert_refused_restore(&built.package, &built._root.path().join("restored"));
    assert_eq!(
        fs::read(&built.record).expect("the packaged record still reads"),
        built.bytes,
        "a refused inspect/restore rewrote the packaged project record"
    );
}

#[test]
fn cad2215_portable_refuses_a_project_shaped_private_policy_without_pm_yaml() {
    let root = refusal_root("no-pm");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    // Control: the same tracker with no PM and no project files exports.
    clean_export(&state, &tracker, &root.path().join("clean"));

    // A tracker that looks like a PM but carries no `pm.yaml`: its project
    // record and issue file are private content the exporter cannot classify
    // by PM root, so they must refuse rather than be copied.
    let project = tracker.join("visible");
    fs::create_dir_all(project.join("VIS-1")).expect("the project issue dir");
    let policy = "key: visible\nprefix: VIS\npolicy:\n  version: 1\n  visibility: private\n";
    fs::write(project.join("project.yaml"), policy).expect("the project record");
    fs::write(
        project.join("VIS-1").join("issue.md"),
        "---\nid: VIS-1\ntitle: PRIVATE_NO_PM_ISSUE\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n",
    )
    .expect("the private issue file");
    commit_tracker(
        &tracker,
        "CAD-2215 project-shaped private policy without pm.yaml",
    );

    refuse(&state, &tracker, &root.path().join("refused"));

    assert_eq!(
        fs::read_to_string(project.join("project.yaml")).expect("the project record still reads"),
        policy,
        "the refused export rewrote the project record"
    );
    assert!(
        project.join("VIS-1").join("issue.md").is_file(),
        "the refused export removed the private issue file"
    );
}

#[test]
fn cad2215_portable_refuses_an_orphan_assist_job_spec() {
    let root = refusal_root("assist-jobs");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    // The daemon-owned spec file for an accepted member request, with no
    // queue row and no job row left to own it: it still contains private work.
    let specs = state.join("assist_jobs");
    fs::create_dir_all(&specs).expect("the assist spec dir");
    let spec = specs.join("assist-cad2215orphan.md");
    let body = "# PRIVATE_ASSIST_SPEC\n\nmember-private job specification\n";
    fs::write(&spec, body).expect("the orphan assist spec");

    refuse(&state, &tracker, &root.path().join("refused"));

    assert_eq!(
        fs::read_to_string(&spec).expect("the orphan spec still reads"),
        body,
        "the refused export rewrote the orphan assist spec"
    );
}

#[test]
fn cad2215_portable_refuses_an_orphan_pm_chat_file_blob() {
    let root = refusal_root("orphan-pm-chat");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &pm.dir, &root.path().join("clean"));

    // PM custody bytes with no `chat_files` row and no staged upload. The
    // directory is gitignored, so the tracker is still "clean" and the
    // exporter would otherwise drop the blob from the package.
    let custody = pm.dir.join(".cadence").join("chat-files");
    fs::create_dir_all(&custody).expect("the PM chat custody dir");
    let blob = custody.join("cad2215-orphan-blob");
    fs::write(&blob, "PRIVATE_PM_CUSTODY_BLOB").expect("the orphan PM custody blob");

    refuse(&state, &pm.dir, &root.path().join("refused"));

    assert_eq!(
        fs::read_to_string(&blob).expect("the orphan blob still reads"),
        "PRIVATE_PM_CUSTODY_BLOB",
        "the refused export rewrote the orphan PM custody blob"
    );
}

#[test]
fn cad2215_portable_refuses_a_chat_upload_account() {
    let root = refusal_root("upload-account");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    // The durable account every operational upload binds to. It names a
    // server-verified principal, so its presence is the reachable marker for
    // uploads whose bytes live in the daemon's separate workspace root.
    let principal = format!("{:x}", Sha256::digest(b"cad2215-member-principal"));
    let account = store_at(&state)
        .chat_upload_account(&principal, "board-member")
        .expect("a real chat upload account");

    refuse(&state, &tracker, &root.path().join("refused"));

    assert_eq!(
        store_at(&state)
            .chat_upload_account(&principal, "board-member")
            .expect("the upload account still reads"),
        account,
        "the refused export removed the chat upload account"
    );
}

#[test]
fn cad2215_portable_refuses_a_nested_pm_legacy_custody_blob() {
    let root = refusal_root("nested-pm-custody");
    let tracker = tracker_fixture(root.path());
    let state = state_with_store(root.path());
    clean_export(&state, &tracker, &root.path().join("clean"));

    // A real nested PM root: `pm/pm.yaml`. The legacy chat custody belongs to
    // that PM workspace (`pm/.cadence/chat-files`), not to the tracker root,
    // and its own `.gitignore` keeps the tracker clean so both public exports
    // would otherwise publish around it.
    let skeleton = Pm::init(&root.path().join("pm-skeleton")).expect("a native PM skeleton");
    fs::create_dir_all(tracker.join("pm")).expect("the nested PM dir");
    fs::copy(
        skeleton.dir.join("pm.yaml"),
        tracker.join("pm").join("pm.yaml"),
    )
    .expect("the nested pm.yaml");
    fs::copy(
        skeleton.dir.join(".gitignore"),
        tracker.join("pm").join(".gitignore"),
    )
    .expect("the nested gitignore");
    let custody = tracker.join("pm").join(".cadence").join("chat-files");
    fs::create_dir_all(&custody).expect("the nested custody dir");
    let blob = custody.join("cad2215-nested-blob");
    fs::write(&blob, "PRIVATE_NESTED_PM_CUSTODY_BLOB").expect("the nested custody blob");
    commit_tracker(&tracker, "CAD-2215 nested PM skeleton");

    refuse(&state, &tracker, &root.path().join("refused-ordinary"));
    assert_eq!(
        fs::read_to_string(&blob).expect("the nested blob still reads"),
        "PRIVATE_NESTED_PM_CUSTODY_BLOB",
        "the refused export rewrote the nested legacy custody blob"
    );

    let snapshot_out = root.path().join("refused-snapshot");
    export_portable_snapshot_selected(&state, &snapshot_out, &export_options(&tracker), &[])
        .expect_err("the snapshot export published around the nested legacy custody blob");
    assert!(
        !snapshot_out.exists(),
        "a refused snapshot export published something"
    );
    assert_eq!(
        fs::read_to_string(&blob).expect("the nested blob still reads"),
        "PRIVATE_NESTED_PM_CUSTODY_BLOB",
        "the refused snapshot export rewrote the nested legacy custody blob"
    );
}

#[test]
fn cad2215_portable_refuses_after_a_restore_leaves_an_operational_upload() {
    let root = refusal_root("restore-upload");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let tracker = pm.dir.clone();
    // Control: this exact state, before any account or upload, exports.
    clean_export(&state, &tracker, &root.path().join("clean"));

    // 1. A verified pre-upload backup, before any account or upload exists.
    let taken = crate::backup::backup(&state, &root.path().join("backups"), 1, "pre-upload")
        .expect("the pre-upload backup");
    let manifest = PathBuf::from(taken["manifest"].as_str().expect("the backup manifest"));

    // 2. The daemon's own sandbox-root resolution, with a workspace that is
    //    distinct from the tracker (the overlap is refused by construction).
    let sandbox = root.path().join("sandbox");
    fs::create_dir_all(&sandbox).expect("the sandbox home root");
    let workspace = crate::home::workspace_for_instance(&tracker, Some(&sandbox), None, None)
        .expect("a real separate operational workspace");
    assert!(
        !workspace.starts_with(&tracker) && !tracker.starts_with(&workspace),
        "the fixture workspace must not overlap the tracker"
    );

    // 3. The upload account and the operational upload it would retain.
    let principal = format!("{:x}", Sha256::digest(b"cad2215-restore-principal"));
    store_at(&state)
        .chat_upload_account(&principal, "board-member")
        .expect("a real upload account");
    let upload = workspace
        .join("users")
        .join("human-cad2215")
        .join("threads")
        .join("thr-cad2215")
        .join("uploads")
        .join("att-cad2215")
        .join("private.txt");
    fs::create_dir_all(upload.parent().expect("the upload parent"))
        .expect("the operational upload dir");
    fs::write(&upload, "PRIVATE_OPERATIONAL_RESTORED_BLOB").expect("the operational upload blob");

    // 4. Restore the pre-upload database over the live one. The account and
    //    chat_files rows disappear; the operational blob does not.
    crate::backup::restore(
        &manifest,
        &state,
        &crate::backup::RestoreOptions {
            force: true,
            repos: Vec::new(),
        },
    )
    .expect("the earlier verified backup restores over the live store");
    assert!(
        store_at(&state)
            .existing_chat_upload_account(&principal)
            .expect("the restored store reads")
            .is_none(),
        "the restore did not remove the upload account"
    );

    // 5. The unprovable operational root is exactly the missing proof: without
    //    the operator's custody statement the export still refuses, and the
    //    blob survives untouched. The statement is the operator's claim and
    //    the package records it as an exclusion; it never proves this root
    //    empty, so this case stays a default-path refusal.
    let mut opts = export_options(&tracker);
    opts.attest_chat_custody_absent = false;
    let out = root.path().join("refused");
    export_portable(&state, &out, &opts)
        .expect_err("an unprovable operational custody root exported");
    assert!(
        !out.exists(),
        "a refused export published {}",
        out.display()
    );
    assert_eq!(
        fs::read_to_string(&upload).expect("the operational blob still reads"),
        "PRIVATE_OPERATIONAL_RESTORED_BLOB",
        "the refused export rewrote the operational upload blob"
    );
}

#[test]
fn cad2215_portable_package_refuses_a_symlinked_member_leaf() {
    let root = refusal_root("package-leaf-symlink");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let package = root.path().join("package");
    clean_export(&state, &pm.dir, &package);

    // The leaf is swapped for a symlink to an external file with identical
    // bytes, so a digest-only check would accept it while reading outside the
    // package.
    let member = package.join("tracker").join("cad").join("project.yaml");
    let bytes = fs::read(&member).expect("the packaged member");
    let external = root.path().join("external-project.yaml");
    fs::write(&external, &bytes).expect("the external same-digest file");
    fs::remove_file(&member).expect("remove the packaged member");
    std::os::unix::fs::symlink(&external, &member).expect("the symlinked member");

    assert!(
        inspect_portable(&package).is_err(),
        "inspect followed a symlinked package member"
    );
    assert_refused_restore(&package, &root.path().join("restored"));
    assert_eq!(
        fs::read(&external).expect("the external file still reads"),
        bytes,
        "the refused inspect/restore touched the external file"
    );
}

#[test]
fn cad2215_portable_package_refuses_a_symlinked_member_ancestor() {
    let root = refusal_root("package-ancestor-symlink");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let package = root.path().join("package");
    clean_export(&state, &pm.dir, &package);

    // The directory above a member is swapped for a symlink to the same tree
    // outside the package: every leaf still stats as a regular file, so only a
    // no-follow package walk can refuse it.
    let project_dir = package.join("tracker").join("cad");
    let external = root.path().join("external-cad");
    fs::rename(&project_dir, &external).expect("move the packaged project dir");
    std::os::unix::fs::symlink(&external, &project_dir).expect("the symlinked ancestor");

    assert!(
        inspect_portable(&package).is_err(),
        "inspect read through a symlinked package directory"
    );
    assert_refused_restore(&package, &root.path().join("restored"));
    assert!(
        external.join("project.yaml").is_file(),
        "the refused inspect/restore removed the external directory"
    );
}

#[test]
fn cad2215_portable_package_refuses_a_non_regular_store_leaf() {
    let root = refusal_root("package-fifo-store");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let package = root.path().join("package");
    clean_export(&state, &pm.dir, &package);

    // A FIFO named like the store: a metadata-only check that later opens it
    // would block (or, after a swap, read a different inode).
    let db = package.join("cadence.sqlite3");
    fs::remove_file(&db).expect("remove the packaged store");
    let c_path = std::ffi::CString::new(db.to_string_lossy().as_bytes()).expect("a fifo path");
    // SAFETY: a fixture path under an owned temp dir; mkfifo only creates it.
    let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(
        made,
        0,
        "the fixture fifo: {}",
        std::io::Error::last_os_error()
    );

    assert!(
        inspect_portable(&package).is_err(),
        "inspect opened a non-regular store member"
    );
    assert_refused_restore(&package, &root.path().join("restored"));
    assert!(
        fs::symlink_metadata(&db).is_ok(),
        "the refused inspect/restore removed the store leaf"
    );
}

#[test]
fn cad2215_portable_package_refuses_a_hardlinked_member() {
    let root = refusal_root("package-hardlink");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let package = root.path().join("package");
    clean_export(&state, &pm.dir, &package);

    // A single-link rule: a hardlink lets the same bytes be mutated through a
    // name outside the package after verification.
    let member = package.join("tracker").join("cad").join("project.yaml");
    let bytes = fs::read(&member).expect("the packaged member");
    let external = root.path().join("external-member.yaml");
    fs::write(&external, &bytes).expect("the external same-digest file");
    fs::remove_file(&member).expect("remove the packaged member");
    fs::hard_link(&external, &member).expect("the hardlinked member");

    assert!(
        inspect_portable(&package).is_err(),
        "inspect accepted a hardlinked package member"
    );
    assert_refused_restore(&package, &root.path().join("restored"));
    assert_eq!(
        fs::read(&external).expect("the external file still reads"),
        bytes,
        "the refused inspect/restore touched the external file"
    );
}

#[test]
fn cad2215_portable_package_refuses_a_sparse_member_over_its_declared_size() {
    let root = refusal_root("package-sparse-member");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let package = root.path().join("package");
    clean_export(&state, &pm.dir, &package);

    // The manifest still declares the member's original small size, but the
    // file on disk is a 1 TiB sparse member. Reading to end would try to
    // materialize it; the guard must refuse from the opened inode's size.
    let member = package.join("tracker").join("cad").join("project.yaml");
    let declared = fs::metadata(&member).expect("the packaged member").len();
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&member)
        .expect("the packaged member opens");
    file.set_len(1 << 40).expect("the sparse member");
    drop(file);

    assert!(
        inspect_portable(&package).is_err(),
        "inspect accepted a sparse member larger than its declared size"
    );
    assert_refused_restore(&package, &root.path().join("restored"));
    assert_eq!(
        fs::metadata(&member)
            .expect("the sparse member still exists")
            .len(),
        1 << 40,
        "the refused inspect/restore rewrote the sparse member"
    );
    assert!(declared < 1 << 40, "the fixture declares a tiny member");
}

#[test]
fn cad2215_portable_package_refuses_an_oversized_manifest() {
    let root = refusal_root("package-oversized-manifest");
    let pm = native_tracker(root.path());
    let state = state_with_store(root.path());
    let package = root.path().join("package");
    clean_export(&state, &pm.dir, &package);

    // A sparse manifest above the 4 MiB transfer cap: the size is read from
    // the opened inode and refused before any allocation or parse.
    let manifest = package.join("manifest.json");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&manifest)
        .expect("the manifest opens");
    file.set_len(8 * 1024 * 1024).expect("the sparse manifest");
    drop(file);

    assert!(
        inspect_portable(&package).is_err(),
        "inspect accepted an oversized manifest"
    );
    assert_refused_restore(&package, &root.path().join("restored"));
    assert_eq!(
        fs::metadata(&manifest)
            .expect("the manifest still exists")
            .len(),
        8 * 1024 * 1024,
        "the refused inspect/restore rewrote the manifest"
    );
}
